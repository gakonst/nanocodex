// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

use image::{DynamicImage, ImageReader};
use ratatui::layout::Size;
use ratatui_image::{
    FontSize, Resize,
    picker::{Picker, ProtocolType},
    sliced::SlicedProtocol,
};
use std::{
    collections::{HashSet, VecDeque},
    env, fs, mem,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime},
};
use url::Url;

pub(super) const MAX_IMAGE_HEIGHT: u16 = 24;

static PICKER: OnceLock<Picker> = OnceLock::new();

pub(super) struct Cache {
    entries: BoundedCache<CacheKey, CachedProtocol, PROTOCOL_CACHE_CAPACITY>,
    sources: BoundedCache<PathBuf, Source, SOURCE_CACHE_CAPACITY>,
    requests: HashSet<CacheKey>,
    completions: mpsc::Receiver<Completion>,
    completion_sender: mpsc::Sender<Completion>,
    epoch: Arc<AtomicU64>,
    active_jobs: Arc<AtomicUsize>,
    generation: u64,
    inline_images: Option<bool>,
    blocked_retry: LayoutChange,
    next_poll: Option<Instant>,
}

pub(super) enum LoadResult {
    Unsupported,
    Deferred,
    Failed,
    Loaded(Arc<SlicedProtocol>),
}

enum CachedProtocol {
    Failed(Fingerprint),
    Loaded {
        protocol: Arc<SlicedProtocol>,
        fingerprint: Fingerprint,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CacheKey {
    path: PathBuf,
    target: Target,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Target {
    Natural { width: u16 },
    Exact { width: u16, height: u16 },
}

#[derive(Clone)]
struct Source {
    image: Arc<DynamicImage>,
    fingerprint: Fingerprint,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Fingerprint(Option<(u64, Option<SystemTime>)>);

struct Completion {
    key: CacheKey,
    generation: u64,
    outcome: CompletionOutcome,
}

enum CompletionOutcome {
    Unchanged,
    Prepared {
        source: Option<Source>,
        protocol: Option<Arc<SlicedProtocol>>,
        fingerprint: Fingerprint,
        invalidates_path: bool,
        refresh: bool,
    },
}

struct BoundedCache<K, V, const CAPACITY: usize>(VecDeque<(K, V)>);

struct JobPermit(Arc<AtomicUsize>);

impl Drop for JobPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

impl<K, V, const CAPACITY: usize> Default for BoundedCache<K, V, CAPACITY> {
    fn default() -> Self {
        Self(VecDeque::new())
    }
}

impl<K: Eq, V, const CAPACITY: usize> BoundedCache<K, V, CAPACITY> {
    fn get(&mut self, key: &K) -> Option<&V> {
        let index = self.0.iter().position(|(candidate, _)| candidate == key)?;
        let entry = self.0.remove(index)?;
        self.0.push_back(entry);
        self.0.back().map(|(_, value)| value)
    }

    fn insert(&mut self, key: K, value: V) {
        self.0.retain(|(candidate, _)| candidate != &key);
        self.0.push_back((key, value));
        if self.0.len() > CAPACITY {
            self.0.pop_front();
        }
    }

    fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
        self.0.retain_mut(|(key, value)| keep(key, value));
    }

    fn clear(&mut self) {
        self.0.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.len()
    }

    #[cfg(test)]
    fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.0.iter().map(|(key, value)| (key, value))
    }
}

#[derive(Default)]
pub(super) struct PollResult {
    pub(super) layout_change: LayoutChange,
    pub(super) render_changed: bool,
}

#[derive(Clone, Copy, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum LayoutChange {
    #[default]
    None,
    Pending,
    Ready,
}

const LOAD_POLL_INTERVAL: Duration = Duration::from_millis(16);
const JOB_QUEUE_CAPACITY: usize = 16;
const SOURCE_CACHE_CAPACITY: usize = 8;
const PROTOCOL_CACHE_CAPACITY: usize = 32;

struct TmuxClient {
    termtype: String,
    font_size: Option<FontSize>,
}

pub(crate) fn initialize() {
    let inside_tmux = env::var_os("TMUX").is_some();
    let tmux_client = inside_tmux.then(tmux_client).flatten();
    let mut picker = if queries_terminal_for_image_capabilities(inside_tmux) {
        Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks())
    } else {
        tmux_client
            .as_ref()
            .and_then(|client| client.font_size)
            .map(picker_from_font_size)
            .unwrap_or_else(Picker::halfblocks)
    };
    let term = env::var("TERM").ok();
    let term_program = env::var("TERM_PROGRAM").ok();
    let native_transport =
        !inside_tmux || picker_supports_tmux_passthrough(term.as_deref(), term_program.as_deref());
    if let Some(protocol) = protocol_override(
        picker.protocol_type(),
        term_program.as_deref(),
        tmux_client.as_ref().map(|client| client.termtype.as_str()),
        inside_tmux,
        native_transport,
    ) {
        picker.set_protocol_type(protocol);
    }
    drop(PICKER.set(picker));
}

const fn queries_terminal_for_image_capabilities(inside_tmux: bool) -> bool {
    !inside_tmux
}

#[allow(deprecated)]
fn picker_from_font_size(font_size: FontSize) -> Picker {
    Picker::from_fontsize(font_size)
}

fn protocol_hint(
    term_program: Option<&str>,
    tmux_client_termtype: Option<&str>,
    native_transport: bool,
) -> Option<ProtocolType> {
    if !native_transport {
        return None;
    }
    [term_program, tmux_client_termtype]
        .into_iter()
        .flatten()
        .filter_map(|terminal| terminal.split_ascii_whitespace().next())
        .any(|terminal| terminal.eq_ignore_ascii_case("ghostty"))
        .then_some(ProtocolType::Kitty)
}

fn protocol_override(
    current: ProtocolType,
    term_program: Option<&str>,
    tmux_client_termtype: Option<&str>,
    inside_tmux: bool,
    native_transport: bool,
) -> Option<ProtocolType> {
    if !native_transport {
        return None;
    }
    if inside_tmux {
        return protocol_hint(None, tmux_client_termtype, true);
    }
    (current == ProtocolType::Halfblocks)
        .then(|| protocol_hint(term_program, None, true))
        .flatten()
}

fn picker_supports_tmux_passthrough(term: Option<&str>, term_program: Option<&str>) -> bool {
    term.is_some_and(|term| term.starts_with("tmux")) || term_program == Some("tmux")
}

fn tmux_client() -> Option<TmuxClient> {
    env::var_os("TMUX")?;
    let output = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "#{client_termtype}\t#{client_cell_width}\t#{client_cell_height}",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let output = String::from_utf8_lossy(&output.stdout);
    parse_tmux_client(&output)
}

fn parse_tmux_client(output: &str) -> Option<TmuxClient> {
    let mut fields = output.trim().split('\t');
    let termtype = fields.next()?.to_owned();
    let width = fields.next().and_then(|value| value.parse().ok());
    let height = fields.next().and_then(|value| value.parse().ok());
    let font_size = width
        .zip(height)
        .filter(|(width, height)| *width > 0 && *height > 0)
        .map(|(width, height)| FontSize::new(width, height));
    Some(TmuxClient {
        termtype,
        font_size,
    })
}

impl Default for Cache {
    fn default() -> Self {
        let (completion_sender, completions) = mpsc::channel();
        Self {
            entries: BoundedCache::default(),
            sources: BoundedCache::default(),
            requests: HashSet::new(),
            completions,
            completion_sender,
            epoch: Arc::new(AtomicU64::new(0)),
            active_jobs: Arc::new(AtomicUsize::new(0)),
            generation: 0,
            inline_images: None,
            blocked_retry: LayoutChange::None,
            next_poll: None,
        }
    }
}

impl Cache {
    pub(super) fn load(&mut self, destination: &str, workspace: &Path, width: u16) -> LoadResult {
        let picker = PICKER.get_or_init(Picker::halfblocks);
        if !self
            .inline_images
            .unwrap_or_else(|| supports_inline_images(picker.protocol_type()))
        {
            return LoadResult::Unsupported;
        }
        let Some(path) = local_path(destination, workspace) else {
            return LoadResult::Failed;
        };
        let key = CacheKey {
            path,
            target: Target::Natural { width },
        };
        self.request(key)
    }

    pub(super) fn clear(&mut self) {
        self.sources.clear();
        self.advance_terminal_generation();
    }

    pub(super) fn retransmit(
        &mut self,
        destination: &str,
        workspace: &Path,
        size: Size,
    ) -> LoadResult {
        let Some(path) = local_path(destination, workspace) else {
            return LoadResult::Failed;
        };
        let key = CacheKey {
            path,
            target: Target::Exact {
                width: size.width,
                height: size.height,
            },
        };
        self.request(key)
    }

    pub(super) fn advance_terminal_generation(&mut self) {
        self.entries.clear();
        self.requests.clear();
        self.generation = self.generation.wrapping_add(1);
        self.epoch.store(self.generation, Ordering::Release);
        self.blocked_retry = LayoutChange::None;
        self.next_poll = None;
    }

    pub(super) const fn animation_deadline(&self) -> Option<Instant> {
        self.next_poll
    }

    pub(super) fn poll(&mut self, now: Instant) -> PollResult {
        let mut result = PollResult::default();
        while let Ok(completion) = self.completions.try_recv() {
            if completion.generation != self.generation {
                continue;
            }
            let path = completion.key.path.clone();
            let requested = self.requests.remove(&completion.key);
            let CompletionOutcome::Prepared {
                source,
                protocol,
                fingerprint,
                invalidates_path,
                refresh,
            } = completion.outcome
            else {
                continue;
            };
            if let Some(source) = source {
                self.sources.insert(path.clone(), source);
            }
            if requested {
                let layout_changed = matches!(completion.key.target, Target::Natural { .. });
                let cached = protocol.map_or_else(
                    || CachedProtocol::Failed(fingerprint),
                    |protocol| CachedProtocol::Loaded {
                        protocol,
                        fingerprint,
                    },
                );
                if invalidates_path {
                    self.entries.retain(|key, _| key.path != path);
                }
                self.entries.insert(completion.key.clone(), cached);
                if layout_changed {
                    result.layout_change = result.layout_change.max(if refresh {
                        LayoutChange::Ready
                    } else {
                        LayoutChange::Pending
                    });
                }
                result.render_changed = true;
            }
        }
        if self.blocked_retry != LayoutChange::None {
            result.layout_change = result.layout_change.max(mem::take(&mut self.blocked_retry));
            result.render_changed = true;
        }
        self.next_poll = (!self.requests.is_empty() || self.blocked_retry != LayoutChange::None)
            .then_some(now + LOAD_POLL_INTERVAL);
        result
    }

    #[cfg(test)]
    pub(super) fn with_inline_images(inline_images: bool) -> Self {
        Self {
            inline_images: Some(inline_images),
            ..Self::default()
        }
    }

    fn request(&mut self, key: CacheKey) -> LoadResult {
        let cached = self.entries.get(&key).map(|cached| match cached {
            CachedProtocol::Failed(fingerprint) => (None, *fingerprint),
            CachedProtocol::Loaded {
                protocol,
                fingerprint,
            } => (Some(Arc::clone(protocol)), *fingerprint),
        });
        let validate = cached.is_none() || matches!(key.target, Target::Natural { .. });
        let blocked = validate
            && !self.requests.contains(&key)
            && (self.requests.len() >= JOB_QUEUE_CAPACITY || !self.spawn(key.clone()));
        if blocked {
            let layout_change = if cached.is_some() {
                LayoutChange::Ready
            } else {
                LayoutChange::Pending
            };
            self.blocked_retry = self.blocked_retry.max(layout_change);
            self.next_poll.get_or_insert_with(Instant::now);
        }
        if let Some((protocol, _)) = cached {
            return protocol.map_or(LoadResult::Failed, LoadResult::Loaded);
        }
        LoadResult::Deferred
    }

    fn spawn(&mut self, key: CacheKey) -> bool {
        let Some(permit) = self.reserve_job() else {
            return false;
        };
        let source = self.sources.get(&key.path).cloned();
        let picker = PICKER.get_or_init(Picker::halfblocks).clone();
        let known_fingerprint = self.entries.get(&key).map(CachedProtocol::fingerprint);
        let epoch = Arc::clone(&self.epoch);
        let generation = self.generation;
        let completion_sender = self.completion_sender.clone();
        self.requests.insert(key.clone());
        self.next_poll.get_or_insert_with(Instant::now);
        rayon::spawn(move || {
            let _permit = permit;
            if epoch.load(Ordering::Acquire) != generation {
                return;
            }
            let completion = prepare(key, generation, source, known_fingerprint, &picker);
            drop(completion_sender.send(completion));
        });
        true
    }

    fn reserve_job(&self) -> Option<JobPermit> {
        self.active_jobs
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < JOB_QUEUE_CAPACITY).then_some(active + 1)
            })
            .ok()
            .map(|_| JobPermit(Arc::clone(&self.active_jobs)))
    }

    #[cfg(test)]
    fn pending_work(&self) -> usize {
        self.requests.len()
    }
}

impl CachedProtocol {
    const fn fingerprint(&self) -> Fingerprint {
        match self {
            Self::Failed(fingerprint) | Self::Loaded { fingerprint, .. } => *fingerprint,
        }
    }
}

fn supports_inline_images(protocol: ProtocolType) -> bool {
    protocol != ProtocolType::Halfblocks
}

fn prepare(
    key: CacheKey,
    generation: u64,
    cached: Option<Source>,
    known_fingerprint: Option<Fingerprint>,
    picker: &Picker,
) -> Completion {
    let metadata = fs::metadata(&key.path).ok();
    let fingerprint = Fingerprint(
        metadata
            .as_ref()
            .map(|metadata| (metadata.len(), metadata.modified().ok())),
    );
    if known_fingerprint == Some(fingerprint) {
        return Completion {
            key,
            generation,
            outcome: CompletionOutcome::Unchanged,
        };
    }
    let invalidates_path = known_fingerprint.is_some_and(|known| known != fingerprint)
        || cached
            .as_ref()
            .is_some_and(|source| source.fingerprint != fingerprint);
    let source = cached
        .filter(|source| source.fingerprint == fingerprint)
        .or_else(|| {
            decode(&key.path).map(|image| Source {
                image: Arc::new(image),
                fingerprint,
            })
        });
    let protocol = source.as_ref().and_then(|source| {
        let size = match key.target {
            Target::Natural { width } => {
                let natural = Resize::natural_size(&source.image, picker.font_size());
                Size::new(
                    natural.width.min(width),
                    natural.height.min(MAX_IMAGE_HEIGHT),
                )
            }
            Target::Exact { width, height } => Size::new(width, height),
        };
        encode(picker, source.image.as_ref().clone(), size)
    });
    Completion {
        key,
        generation,
        outcome: CompletionOutcome::Prepared {
            source,
            protocol,
            fingerprint,
            invalidates_path,
            refresh: known_fingerprint.is_some(),
        },
    }
}

fn decode(path: &Path) -> Option<DynamicImage> {
    let image = ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    Some(image)
}

fn encode(picker: &Picker, image: DynamicImage, size: Size) -> Option<Arc<SlicedProtocol>> {
    SlicedProtocol::new_with_resize(picker, image, size, Resize::Fit(None))
        .ok()
        .map(Arc::new)
}

fn local_path(destination: &str, workspace: &Path) -> Option<PathBuf> {
    let base = Url::from_directory_path(workspace).ok()?;
    let destination = base.join(destination).ok()?;
    if destination.scheme() != "file" {
        return None;
    }
    destination.to_file_path().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        Cache, JOB_QUEUE_CAPACITY, LayoutChange, LoadResult, PROTOCOL_CACHE_CAPACITY,
        SOURCE_CACHE_CAPACITY, Target, picker_supports_tmux_passthrough, protocol_hint,
        protocol_override, queries_terminal_for_image_capabilities, supports_inline_images,
    };
    use ratatui::layout::Size;
    use ratatui_image::picker::ProtocolType;
    use std::{
        fs::File,
        sync::Arc,
        time::{Duration, Instant},
    };

    fn write_png(path: &std::path::Path) {
        write_png_size(path, 8, 8);
    }

    fn write_png_size(path: &std::path::Path, width: u32, height: u32) {
        let file = File::create(path).unwrap();
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&[0xff, 0, 0].repeat((width * height) as usize))
            .unwrap();
    }

    fn wait_for_load(
        cache: &mut Cache,
        destination: &str,
        workspace: &std::path::Path,
        width: u16,
    ) -> std::sync::Arc<ratatui_image::sliced::SlicedProtocol> {
        eventually(|| {
            cache.poll(Instant::now());
            loaded(cache.load(destination, workspace, width))
        })
    }

    fn eventually<T>(mut check: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(value) = check() {
                return value;
            }
            assert!(Instant::now() < deadline, "image preparation timed out");
            std::thread::yield_now();
        }
    }

    fn loaded(result: LoadResult) -> Option<Arc<ratatui_image::sliced::SlicedProtocol>> {
        match result {
            LoadResult::Loaded(protocol) => Some(protocol),
            _ => None,
        }
    }

    #[test]
    fn ghostty_protocol_detection_respects_tmux_transport() {
        assert_eq!(
            protocol_hint(Some("ghostty"), None, true),
            Some(ProtocolType::Kitty)
        );
        assert_eq!(
            protocol_hint(Some("tmux"), Some("ghostty 1.3.1"), true),
            Some(ProtocolType::Kitty)
        );
        assert!(!picker_supports_tmux_passthrough(
            Some("screen-256color"),
            Some("ghostty")
        ));
        assert_eq!(
            protocol_hint(Some("ghostty"), Some("ghostty 1.3.1"), false),
            None
        );
    }

    #[test]
    fn tmux_image_initialization_does_not_block_on_terminal_queries() {
        assert!(!queries_terminal_for_image_capabilities(true));
        assert!(queries_terminal_for_image_capabilities(false));
    }

    #[test]
    fn tmux_client_dimensions_supply_the_outer_terminal_font_size() {
        let client = super::parse_tmux_client("ghostty 1.3.1\t22\t49\n").unwrap();

        assert_eq!(client.termtype, "ghostty 1.3.1");
        let font_size = client.font_size.unwrap();
        assert_eq!((font_size.width, font_size.height), (22, 49));
    }

    #[test]
    fn current_tmux_client_overrides_stale_terminal_protocol() {
        assert_eq!(
            protocol_override(
                ProtocolType::Iterm2,
                Some("iTerm.app"),
                Some("ghostty 1.3.1"),
                true,
                true,
            ),
            Some(ProtocolType::Kitty),
        );
    }

    #[test]
    fn halfblocks_are_not_an_inline_image_backend() {
        assert!(!supports_inline_images(ProtocolType::Halfblocks));
        for protocol in [
            ProtocolType::Kitty,
            ProtocolType::Sixel,
            ProtocolType::Iterm2,
        ] {
            assert!(supports_inline_images(protocol));
        }
    }

    #[test]
    fn image_preparation_is_deferred_off_the_render_thread() {
        let workspace = tempfile::tempdir().unwrap();
        write_png(&workspace.path().join("sample.png"));
        let mut cache = Cache::with_inline_images(true);

        assert!(matches!(
            cache.load("sample.png", workspace.path(), 20),
            LoadResult::Deferred
        ));
        assert!(matches!(
            cache.retransmit("sample.png", workspace.path(), Size::new(4, 2)),
            LoadResult::Deferred
        ));
    }

    #[test]
    fn natural_and_exact_size_variants_prepare_independently() {
        let workspace = tempfile::tempdir().unwrap();
        write_png_size(&workspace.path().join("sample.png"), 80, 40);
        let mut cache = Cache::with_inline_images(true);
        let exact_sizes = [Size::new(4, 2), Size::new(8, 4)];

        for width in [12, 24] {
            assert!(matches!(
                cache.load("sample.png", workspace.path(), width),
                LoadResult::Deferred
            ));
        }
        for size in exact_sizes {
            assert!(matches!(
                cache.retransmit("sample.png", workspace.path(), size),
                LoadResult::Deferred
            ));
        }

        let narrow = wait_for_load(&mut cache, "sample.png", workspace.path(), 12);
        let wide = wait_for_load(&mut cache, "sample.png", workspace.path(), 24);
        let first_exact = eventually(|| {
            cache.poll(Instant::now());
            loaded(cache.retransmit("sample.png", workspace.path(), exact_sizes[0]))
        });
        let second_exact = eventually(|| {
            cache.poll(Instant::now());
            loaded(cache.retransmit("sample.png", workspace.path(), exact_sizes[1]))
        });

        assert!(!Arc::ptr_eq(&narrow, &wide));
        assert!(!Arc::ptr_eq(&first_exact, &second_exact));
        assert!(Arc::ptr_eq(
            &narrow,
            &wait_for_load(&mut cache, "sample.png", workspace.path(), 12)
        ));
        assert_eq!(
            cache
                .entries
                .iter()
                .filter(|(key, _)| matches!(key.target, Target::Exact { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn changed_and_newly_created_images_replace_cached_results() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("sample.png");
        write_png(&path);
        let mut cache = Cache::with_inline_images(true);
        let original = wait_for_load(&mut cache, "sample.png", workspace.path(), 20);

        write_png_size(&path, 80, 40);
        let changed = eventually(|| {
            cache.poll(Instant::now());
            loaded(cache.load("sample.png", workspace.path(), 20))
                .filter(|protocol| !Arc::ptr_eq(&original, protocol))
        });
        assert!(!Arc::ptr_eq(&original, &changed));

        let missing = "created-later.png";
        eventually(|| {
            cache.poll(Instant::now());
            matches!(
                cache.load(missing, workspace.path(), 20),
                LoadResult::Failed
            )
            .then_some(())
        });
        write_png(&workspace.path().join(missing));
        let _ = wait_for_load(&mut cache, missing, workspace.path(), 20);
    }

    #[test]
    fn decoded_sources_and_prepared_protocols_are_bounded() {
        let workspace = tempfile::tempdir().unwrap();
        let mut cache = Cache::with_inline_images(true);
        for index in 0..PROTOCOL_CACHE_CAPACITY + 4 {
            let name = format!("sample-{index}.png");
            write_png(&workspace.path().join(&name));
            let _ = wait_for_load(&mut cache, &name, workspace.path(), 20);
        }

        assert!(cache.sources.len() <= SOURCE_CACHE_CAPACITY);
        assert!(cache.entries.len() <= PROTOCOL_CACHE_CAPACITY);
    }

    #[test]
    fn image_work_is_bounded_and_rejected_images_retry() {
        let workspace = tempfile::tempdir().unwrap();
        let mut cache = Cache::with_inline_images(true);

        for index in 0..JOB_QUEUE_CAPACITY {
            let name = format!("queued-{index}.png");
            write_png(&workspace.path().join(&name));
            assert!(matches!(
                cache.retransmit(&name, workspace.path(), Size::new(4, 2)),
                LoadResult::Deferred
            ));
        }
        write_png(&workspace.path().join("visible.png"));
        assert!(matches!(
            cache.load("visible.png", workspace.path(), 20),
            LoadResult::Deferred
        ));
        assert_eq!(cache.pending_work(), JOB_QUEUE_CAPACITY);

        eventually(|| {
            (cache.poll(Instant::now()).layout_change == LayoutChange::Pending).then_some(())
        });
        eventually(|| {
            cache.poll(Instant::now());
            loaded(cache.load("visible.png", workspace.path(), 20))
        });
    }

    #[test]
    fn rejected_cached_validation_retries_when_capacity_opens() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("sample.png");
        write_png(&path);
        let mut cache = Cache::with_inline_images(true);
        let original = wait_for_load(&mut cache, "sample.png", workspace.path(), 20);
        eventually(|| {
            cache.poll(Instant::now());
            (cache.pending_work() == 0).then_some(())
        });
        let permits = (0..JOB_QUEUE_CAPACITY)
            .map(|_| cache.reserve_job().unwrap())
            .collect::<Vec<_>>();

        write_png_size(&path, 80, 40);
        let cached = loaded(cache.load("sample.png", workspace.path(), 20)).unwrap();
        assert!(Arc::ptr_eq(&original, &cached));
        drop(permits);

        assert!(matches!(
            cache.poll(Instant::now()).layout_change,
            LayoutChange::Ready
        ));

        eventually(|| {
            cache.poll(Instant::now());
            loaded(cache.load("sample.png", workspace.path(), 20))
                .filter(|protocol| !Arc::ptr_eq(&original, protocol))
        });
    }

    #[test]
    fn image_work_permits_survive_generation_changes() {
        let mut cache = Cache::with_inline_images(true);
        let permits = (0..JOB_QUEUE_CAPACITY)
            .map(|_| cache.reserve_job().unwrap())
            .collect::<Vec<_>>();

        cache.advance_terminal_generation();
        assert!(cache.reserve_job().is_none());

        drop(permits);
        assert!(cache.reserve_job().is_some());
    }
}
