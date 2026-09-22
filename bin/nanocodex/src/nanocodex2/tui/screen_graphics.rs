//! Local Kitty transfers keep pixel data out of the terminal/tmux byte stream.
//! The terminal reads and deletes each private temporary RGB file. Unicode
//! placements follow the pane's cells, including zoom, scrolling and closing.
use super::Result;

const VIDEO_IMAGE_ID: u32 = 0x4e430001;
use base64::Engine;
use image::DynamicImage;
use ratatui::{
    buffer::{Buffer, CellDiffOption},
    layout::{Rect, Size},
    widgets::Widget,
};
use ratatui_image::{
    FontSize,
    picker::{Picker, ProtocolType},
    protocol::Protocol,
};
use std::{
    fmt::Write as _,
    io::Write as _,
    num::NonZeroU16,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) enum VideoFrame {
    Protocol(Protocol),
    Local(LocalFrame),
}
#[cfg(test)]
impl VideoFrame {
    pub(super) fn consume_local_pixels(&self) {
        if let Self::Local(frame) = self
            && let Some(path) = &frame.path
        {
            let bytes = std::fs::read(path).unwrap();
            assert!(!bytes.is_empty());
            std::fs::remove_file(path).unwrap();
        }
    }
}
impl Widget for &VideoFrame {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        match self {
            VideoFrame::Protocol(protocol) => {
                ratatui_image::Image::new(protocol).render(area, buffer)
            }
            VideoFrame::Local(frame) => frame.render(area, buffer),
        }
    }
}
pub(super) struct LocalGraphics {
    directory: tempfile::TempDir,
    pending: Mutex<Vec<(PathBuf, usize)>>,
    tmux: bool,
}
impl LocalGraphics {
    pub fn new(picker: &Picker) -> Option<Arc<Self>> {
        if picker.protocol_type() != ProtocolType::Kitty
            || std::env::var_os("SSH_CONNECTION").is_some()
            || std::env::var_os("SSH_TTY").is_some()
        {
            return None;
        }
        Self::create(std::env::var_os("TMUX").is_some()).ok()
    }
    fn create(tmux: bool) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            directory: tempfile::Builder::new()
                .prefix("tty-graphics-protocol-nanocodex-")
                .tempdir()?,
            pending: Mutex::new(Vec::new()),
            tmux,
        }))
    }
    pub fn prepare(
        self: &Arc<Self>,
        image: &DynamicImage,
        area: Size,
        font: FontSize,
    ) -> Result<Option<VideoFrame>> {
        let bytes = image.width() as usize * image.height() as usize * 3;
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "Graphics queue unavailable")?;
        pending.retain(|(path, _)| path.exists());
        // Missing file-transfer support must neither leak files nor stop the
        // viewer. Fall back to inline graphics if the terminal stops consuming.
        if pending.len() >= 8
            || pending.iter().map(|(_, size)| size).sum::<usize>() + bytes > 128 * 1024 * 1024
        {
            return Ok(None);
        }
        let mut file = tempfile::NamedTempFile::new_in(self.directory.path())?;
        if let Some(rgb) = image.as_rgb8() {
            file.write_all(rgb.as_raw())?;
        } else {
            file.write_all(image.to_rgb8().as_raw())?;
        }
        let (file, path) = file.keep()?;
        drop(file);
        pending.push((path.clone(), bytes));
        let size = fit_cells(image.width(), image.height(), area, font);
        // Keep both the image and virtual placement stable. Anonymous placements
        // accumulate in Ghostty, and alternating IDs repaints every text row.
        let id = VIDEO_IMAGE_ID;
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(path.to_string_lossy().as_bytes());
        let command = format!(
            "\x1b_Gq=2,i={id},p=1,a=T,U=1,f=24,t=t,s={},v={},c={},r={};{encoded}\x1b\\",
            image.width(),
            image.height(),
            size.width,
            size.height
        );
        let command = if self.tmux {
            format!("\x1bPtmux;{}\x1b\\", command.replace('\x1b', "\x1b\x1b"))
        } else {
            command
        };
        Ok(Some(VideoFrame::Local(LocalFrame {
            _owner: Some(self.clone()),
            path: Some(path),
            command,
            id,
            size,
            transmitted: AtomicBool::new(false),
        })))
    }
}
/// Same placement as local transfers, for SSH or terminals without file access.
pub(super) fn inline(image: &DynamicImage, area: Size, font: FontSize, tmux: bool) -> VideoFrame {
    let pixels = image.to_rgb8();
    let size = fit_cells(image.width(), image.height(), area, font);
    let mut command = String::new();
    let count = pixels.as_raw().len().div_ceil(3072);
    for (index, chunk) in pixels.as_raw().chunks(3072).enumerate() {
        let mut part = String::from("\x1b_Gq=2,");
        if index == 0 {
            write!(
                part,
                "i={VIDEO_IMAGE_ID},p=1,a=T,U=1,f=24,t=d,s={},v={},c={},r={},",
                image.width(),
                image.height(),
                size.width,
                size.height
            )
            .unwrap();
        }
        write!(part, "m={};", u8::from(index + 1 < count)).unwrap();
        base64::engine::general_purpose::STANDARD.encode_string(chunk, &mut part);
        part.push_str("\x1b\\");
        if tmux {
            write!(
                command,
                "\x1bPtmux;{}\x1b\\",
                part.replace('\x1b', "\x1b\x1b")
            )
            .unwrap();
        } else {
            command.push_str(&part);
        }
    }
    VideoFrame::Local(LocalFrame {
        _owner: None,
        path: None,
        command,
        id: VIDEO_IMAGE_ID,
        size,
        transmitted: AtomicBool::new(false),
    })
}

fn fit_cells(width: u32, height: u32, area: Size, font: FontSize) -> Size {
    let scale = (f64::from(area.width) * f64::from(font.width) / f64::from(width))
        .min(f64::from(area.height) * f64::from(font.height) / f64::from(height));
    Size::new(
        ((f64::from(width) * scale / f64::from(font.width)).floor() as u16)
            .max(1)
            .min(area.width),
        ((f64::from(height) * scale / f64::from(font.height)).floor() as u16)
            .max(1)
            .min(area.height),
    )
}
pub(crate) struct LocalFrame {
    _owner: Option<Arc<LocalGraphics>>,
    path: Option<PathBuf>,
    command: String,
    id: u32,
    size: Size,
    transmitted: AtomicBool,
}
impl Drop for LocalFrame {
    fn drop(&mut self) {
        // Unpresented frames never reach the terminal, so we own their cleanup.
        // Presented files belong to the terminal until it reads/unlinks them;
        // the bounded session directory also reclaims them on close.
        if !self.transmitted.load(Ordering::Acquire)
            && let Some(path) = &self.path
        {
            let _ = std::fs::remove_file(path);
        }
    }
}
impl LocalFrame {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        let width = self.size.width.min(area.width);
        let height = self
            .size
            .height
            .min(area.height)
            .min(DIACRITICS.len() as u16);
        if width == 0 || height == 0 {
            return;
        }
        let transmit = !self.transmitted.swap(true, Ordering::AcqRel);
        let [extra, r, g, b] = self.id.to_be_bytes();
        for y in 0..height {
            let mut row = String::new();
            if y == 0 && transmit {
                row.push_str(&self.command);
            }
            // Same virtual-placement convention as ratatui-image's Kitty
            // renderer; source pixels are scaled by the terminal's GPU.
            write!(
                row,
                "\x1b[s\x1b[38;2;{r};{g};{b}m\x1b[58;2;0;0;1m\u{10eeee}{}{}{}",
                DIACRITICS[y as usize], DIACRITICS[0], DIACRITICS[extra as usize]
            )
            .unwrap();
            for _ in 1..width {
                row.push('\u{10eeee}');
            }
            // Match the one-cell advance promised to Ratatui by ForcedWidth.
            row.push_str("\x1b[u\x1b[1C");
            for x in 1..width {
                buffer[(area.x + x, area.y + y)].set_diff_option(CellDiffOption::Skip);
            }
            buffer[(area.x, area.y + y)]
                .set_symbol(&row)
                .set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::new(1).unwrap()));
        }
    }
}

// Kitty protocol row/column diacritics, from the protocol specification.
// https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders
static DIACRITICS: [char; 297] = [
    '\u{305}',
    '\u{30D}',
    '\u{30E}',
    '\u{310}',
    '\u{312}',
    '\u{33D}',
    '\u{33E}',
    '\u{33F}',
    '\u{346}',
    '\u{34A}',
    '\u{34B}',
    '\u{34C}',
    '\u{350}',
    '\u{351}',
    '\u{352}',
    '\u{357}',
    '\u{35B}',
    '\u{363}',
    '\u{364}',
    '\u{365}',
    '\u{366}',
    '\u{367}',
    '\u{368}',
    '\u{369}',
    '\u{36A}',
    '\u{36B}',
    '\u{36C}',
    '\u{36D}',
    '\u{36E}',
    '\u{36F}',
    '\u{483}',
    '\u{484}',
    '\u{485}',
    '\u{486}',
    '\u{487}',
    '\u{592}',
    '\u{593}',
    '\u{594}',
    '\u{595}',
    '\u{597}',
    '\u{598}',
    '\u{599}',
    '\u{59C}',
    '\u{59D}',
    '\u{59E}',
    '\u{59F}',
    '\u{5A0}',
    '\u{5A1}',
    '\u{5A8}',
    '\u{5A9}',
    '\u{5AB}',
    '\u{5AC}',
    '\u{5AF}',
    '\u{5C4}',
    '\u{610}',
    '\u{611}',
    '\u{612}',
    '\u{613}',
    '\u{614}',
    '\u{615}',
    '\u{616}',
    '\u{617}',
    '\u{657}',
    '\u{658}',
    '\u{659}',
    '\u{65A}',
    '\u{65B}',
    '\u{65D}',
    '\u{65E}',
    '\u{6D6}',
    '\u{6D7}',
    '\u{6D8}',
    '\u{6D9}',
    '\u{6DA}',
    '\u{6DB}',
    '\u{6DC}',
    '\u{6DF}',
    '\u{6E0}',
    '\u{6E1}',
    '\u{6E2}',
    '\u{6E4}',
    '\u{6E7}',
    '\u{6E8}',
    '\u{6EB}',
    '\u{6EC}',
    '\u{730}',
    '\u{732}',
    '\u{733}',
    '\u{735}',
    '\u{736}',
    '\u{73A}',
    '\u{73D}',
    '\u{73F}',
    '\u{740}',
    '\u{741}',
    '\u{743}',
    '\u{745}',
    '\u{747}',
    '\u{749}',
    '\u{74A}',
    '\u{7EB}',
    '\u{7EC}',
    '\u{7ED}',
    '\u{7EE}',
    '\u{7EF}',
    '\u{7F0}',
    '\u{7F1}',
    '\u{7F3}',
    '\u{816}',
    '\u{817}',
    '\u{818}',
    '\u{819}',
    '\u{81B}',
    '\u{81C}',
    '\u{81D}',
    '\u{81E}',
    '\u{81F}',
    '\u{820}',
    '\u{821}',
    '\u{822}',
    '\u{823}',
    '\u{825}',
    '\u{826}',
    '\u{827}',
    '\u{829}',
    '\u{82A}',
    '\u{82B}',
    '\u{82C}',
    '\u{82D}',
    '\u{951}',
    '\u{953}',
    '\u{954}',
    '\u{F82}',
    '\u{F83}',
    '\u{F86}',
    '\u{F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn successive_frames_replace_one_placement_without_repainting_image_rows() {
        let graphics = LocalGraphics::create(false).unwrap();
        let area = Rect::new(3, 2, 20, 10);
        let font = FontSize {
            width: 8,
            height: 16,
        };
        let first = graphics
            .prepare(&DynamicImage::new_rgb8(160, 160), area.as_size(), font)
            .unwrap()
            .unwrap();
        let second = graphics
            .prepare(&DynamicImage::new_rgb8(160, 160), area.as_size(), font)
            .unwrap()
            .unwrap();
        let mut before = Buffer::empty(area);
        let mut after = Buffer::empty(area);
        (&first).render(area, &mut before);
        (&second).render(area, &mut after);
        let changed: Vec<_> = before
            .diff(&after)
            .into_iter()
            .map(|(x, y, _)| (x, y))
            .collect();
        assert_eq!(changed, vec![(area.x, area.y)]);
        let row = after[(area.x, area.y)].symbol();
        assert!(row.contains(&format!("i={VIDEO_IMAGE_ID},p=1,a=T,U=1")));
        assert!(row.contains("\x1b[58;2;0;0;1m"));
        assert!(row.ends_with("\x1b[u\x1b[1C"));
        // Ordinary redraws never re-read a file that the terminal has deleted.
        let mut redraw = Buffer::empty(area);
        (&second).render(area, &mut redraw);
        assert!(!redraw[(area.x, area.y)].symbol().contains("\x1b_G"));
    }

    #[test]
    fn inline_transfer_uses_the_same_named_placement_and_bounded_chunks() {
        let image = DynamicImage::new_rgb8(100, 100);
        let frame = inline(
            &image,
            Size::new(20, 10),
            FontSize {
                width: 8,
                height: 16,
            },
            false,
        );
        let VideoFrame::Local(frame) = frame else {
            panic!("Kitty frame expected")
        };
        assert!(
            frame
                .command
                .contains(&format!("i={VIDEO_IMAGE_ID},p=1,a=T,U=1,f=24,t=d"))
        );
        let mut decoded = Vec::new();
        for part in frame
            .command
            .split("\x1b\\")
            .filter(|part| !part.is_empty())
        {
            let (_, payload) = part.split_once(';').unwrap();
            assert!(payload.len() <= 4096);
            decoded.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(payload)
                    .unwrap(),
            );
        }
        assert_eq!(decoded, image.to_rgb8().into_raw());
        assert!(frame.command.rsplit_once(';').unwrap().0.ends_with("m=0"));
    }

    #[test]
    fn local_pixels_remain_outside_terminal_text_and_unpresented_frames_are_reclaimed() {
        let graphics = LocalGraphics::create(true).unwrap();
        let image = DynamicImage::new_rgb8(1920, 1080);
        let frame = graphics
            .prepare(
                &image,
                Size::new(120, 40),
                FontSize {
                    width: 8,
                    height: 16,
                },
            )
            .unwrap()
            .unwrap();
        let VideoFrame::Local(local) = &frame else {
            panic!("local transfer expected")
        };
        assert!(local.command.starts_with("\x1bPtmux;\x1b\x1b_G"));
        assert!(local.command.contains("f=24,t=t,s=1920,v=1080,c=120,r=33"));
        assert!(local.command.len() < 1024);
        let path = local.path.clone().unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 1920 * 1080 * 3);
        drop(frame);
        assert!(!path.exists());
    }
    #[test]
    fn transmitted_files_live_until_terminal_consumption_and_queue_is_bounded() {
        let graphics = LocalGraphics::create(false).unwrap();
        let image = DynamicImage::new_rgb8(16, 16);
        let area = Rect::new(0, 0, 10, 4);
        let mut buffer = Buffer::empty(area);
        for _ in 0..8 {
            let frame = graphics
                .prepare(
                    &image,
                    area.as_size(),
                    FontSize {
                        width: 8,
                        height: 16,
                    },
                )
                .unwrap()
                .unwrap();
            (&frame).render(area, &mut buffer);
        }
        assert_eq!(
            std::fs::read_dir(graphics.directory.path())
                .unwrap()
                .count(),
            8
        );
        assert!(
            graphics
                .prepare(
                    &image,
                    area.as_size(),
                    FontSize {
                        width: 8,
                        height: 16
                    },
                )
                .unwrap()
                .is_none()
        );
        let path = graphics.directory.path().to_owned();
        drop(graphics);
        assert!(!path.exists());
    }
}
