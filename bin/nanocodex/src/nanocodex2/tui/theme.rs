// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Configurable terminal colors and light/dark mode selection.

use crate::config::ReasoningEffort;
use nanocodex::Model;
use ratatui::style::Color;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{fmt, str::FromStr};
use tokio::{sync::mpsc, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ThemeMode {
    #[default]
    Auto,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ColorScheme {
    Light,
    Dark,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Theme {
    mode: ThemeMode,
    light: ThemePalette,
    dark: ThemePalette,
    #[serde(skip)]
    system_scheme: ColorScheme,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ThemePalette {
    text: ThemeColor,
    border: ThemeColor,
    muted: ThemeColor,
    accent: ThemeColor,
    code_text: ThemeColor,
    code_background: ThemeColor,
    thinking_low: ThemeColor,
    thinking_medium: ThemeColor,
    thinking_high: ThemeColor,
    thinking_xhigh: ThemeColor,
    thinking_max: ThemeColor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ThemeColor(Color);

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ThemeFields {
    mode: ThemeMode,
    light: PaletteFields,
    dark: PaletteFields,
    #[serde(flatten)]
    shared: PaletteFields,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PaletteFields {
    text: Option<ThemeColor>,
    border: Option<ThemeColor>,
    muted: Option<ThemeColor>,
    accent: Option<ThemeColor>,
    code_text: Option<ThemeColor>,
    code_background: Option<ThemeColor>,
    thinking_low: Option<ThemeColor>,
    thinking_medium: Option<ThemeColor>,
    thinking_high: Option<ThemeColor>,
    thinking_xhigh: Option<ThemeColor>,
    thinking_max: Option<ThemeColor>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ColorValue {
    Name(String),
    Index(u8),
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            mode: ThemeMode::Auto,
            light: ThemePalette::light(),
            dark: ThemePalette::dark(),
            system_scheme: ColorScheme::Dark,
        }
    }
}

impl Theme {
    pub(crate) const fn mode(&self) -> ThemeMode {
        self.mode
    }

    pub(crate) fn set_mode(&mut self, mode: ThemeMode) {
        self.mode = mode;
    }

    pub(crate) fn set_system_scheme(&mut self, scheme: ColorScheme) -> bool {
        if self.system_scheme == scheme {
            return false;
        }
        self.system_scheme = scheme;
        self.mode == ThemeMode::Auto
    }

    pub(crate) fn replace_from_config(&mut self, mut theme: Self) {
        theme.system_scheme = self.system_scheme;
        *self = theme;
    }

    pub(crate) const fn text(&self) -> Color {
        self.palette().text.0
    }

    pub(crate) const fn border(&self) -> Color {
        self.palette().border.0
    }

    pub(crate) const fn muted(&self) -> Color {
        self.palette().muted.0
    }

    pub(crate) const fn accent(&self) -> Color {
        self.palette().accent.0
    }

    pub(crate) const fn code_text(&self) -> Color {
        self.palette().code_text.0
    }

    pub(crate) const fn code_background(&self) -> Color {
        self.palette().code_background.0
    }

    pub(crate) const fn thinking_low(&self) -> Color {
        self.palette().thinking_low.0
    }

    pub(crate) const fn thinking_medium(&self) -> Color {
        self.palette().thinking_medium.0
    }

    pub(crate) const fn thinking_high(&self) -> Color {
        self.palette().thinking_high.0
    }

    pub(crate) const fn thinking_xhigh(&self) -> Color {
        self.palette().thinking_xhigh.0
    }

    pub(crate) const fn thinking_max(&self) -> Color {
        self.palette().thinking_max.0
    }

    pub(crate) const fn effort(&self, effort: ReasoningEffort) -> Color {
        match effort {
            ReasoningEffort::Low => self.thinking_low(),
            ReasoningEffort::Medium => self.thinking_medium(),
            ReasoningEffort::High => self.thinking_high(),
            ReasoningEffort::Xhigh => self.thinking_xhigh(),
            ReasoningEffort::Max => self.thinking_max(),
        }
    }

    pub(crate) const fn model(&self, model: Model) -> Color {
        match model {
            Model::Luna => Color::White,
            Model::Terra => Color::Green,
            Model::Sol => Color::Yellow,
            Model::Astra => Color::LightMagenta,
        }
    }

    const fn palette(&self) -> &ThemePalette {
        match self.mode {
            ThemeMode::Light => &self.light,
            ThemeMode::Dark => &self.dark,
            ThemeMode::Auto => match self.system_scheme {
                ColorScheme::Light => &self.light,
                ColorScheme::Dark => &self.dark,
            },
        }
    }
}

impl ThemeMode {
    pub(crate) const ALL: [Self; 3] = [Self::Auto, Self::Light, Self::Dark];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

impl ThemePalette {
    const fn dark() -> Self {
        Self {
            text: ThemeColor(Color::Reset),
            border: ThemeColor(Color::DarkGray),
            muted: ThemeColor(Color::DarkGray),
            accent: ThemeColor(Color::Blue),
            code_text: ThemeColor(Color::Rgb(0xD7, 0xD7, 0xD7)),
            code_background: ThemeColor(Color::Rgb(0x26, 0x26, 0x26)),
            thinking_low: ThemeColor(Color::Gray),
            thinking_medium: ThemeColor(Color::Cyan),
            thinking_high: ThemeColor(Color::Yellow),
            thinking_xhigh: ThemeColor(Color::Red),
            thinking_max: ThemeColor(Color::Magenta),
        }
    }

    const fn light() -> Self {
        Self {
            text: ThemeColor(Color::Reset),
            border: ThemeColor(Color::DarkGray),
            muted: ThemeColor(Color::DarkGray),
            accent: ThemeColor(Color::Blue),
            code_text: ThemeColor(Color::Rgb(0x26, 0x26, 0x26)),
            code_background: ThemeColor(Color::Rgb(0xEE, 0xEE, 0xEE)),
            thinking_low: ThemeColor(Color::DarkGray),
            thinking_medium: ThemeColor(Color::Rgb(0x00, 0x78, 0x78)),
            thinking_high: ThemeColor(Color::Rgb(0x9A, 0x67, 0x00)),
            thinking_xhigh: ThemeColor(Color::Red),
            thinking_max: ThemeColor(Color::Magenta),
        }
    }

    fn apply(&mut self, fields: &PaletteFields) {
        self.text = fields.text.unwrap_or(self.text);
        self.border = fields.border.unwrap_or(self.border);
        self.muted = fields.muted.unwrap_or(self.muted);
        self.accent = fields.accent.unwrap_or(self.accent);
        self.code_text = fields.code_text.unwrap_or(self.code_text);
        self.code_background = fields.code_background.unwrap_or(self.code_background);
        self.thinking_low = fields.thinking_low.unwrap_or(self.thinking_low);
        self.thinking_medium = fields.thinking_medium.unwrap_or(self.thinking_medium);
        self.thinking_high = fields.thinking_high.unwrap_or(self.thinking_high);
        self.thinking_xhigh = fields.thinking_xhigh.unwrap_or(self.thinking_xhigh);
        self.thinking_max = fields.thinking_max.unwrap_or(self.thinking_max);
    }
}

impl<'de> Deserialize<'de> for Theme {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = ThemeFields::deserialize(deserializer)?;
        let mut light = ThemePalette::light();
        light.apply(&fields.shared);
        light.apply(&fields.light);
        let mut dark = ThemePalette::dark();
        dark.apply(&fields.shared);
        dark.apply(&fields.dark);
        Ok(Self {
            mode: fields.mode,
            light,
            dark,
            system_scheme: ColorScheme::Dark,
        })
    }
}

impl<'de> Deserialize<'de> for ThemeColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = ColorValue::deserialize(deserializer)?;
        match value {
            ColorValue::Name(value) => Color::from_str(&value)
                .map(Self)
                .map_err(|_| de::Error::custom(format!("invalid terminal color `{value}`"))),
            ColorValue::Index(value) => Ok(Self(Color::Indexed(value))),
        }
    }
}

impl Serialize for ThemeColor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&ColorName(self.0).to_string())
    }
}

pub(crate) fn detect_system_scheme() -> Option<ColorScheme> {
    match dark_light::detect().ok()? {
        dark_light::Mode::Light => Some(ColorScheme::Light),
        dark_light::Mode::Dark => Some(ColorScheme::Dark),
        dark_light::Mode::Unspecified => None,
    }
}

const SYSTEM_SCHEME_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) fn watch_system_scheme(
    updates: mpsc::UnboundedSender<ColorScheme>,
    shutdown: CancellationToken,
) {
    tokio::spawn(async move {
        let mut last = None;
        let mut interval = tokio::time::interval(SYSTEM_SCHEME_POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    let detected = tokio::task::spawn_blocking(detect_system_scheme)
                        .await
                        .ok()
                        .flatten();
                    if let Some(scheme) = detected
                        && last != Some(scheme)
                    {
                        last = Some(scheme);
                        if updates.send(scheme).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });
}

struct ColorName(Color);

impl fmt::Display for ColorName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Color::Reset => formatter.write_str("reset"),
            Color::Black => formatter.write_str("black"),
            Color::Red => formatter.write_str("red"),
            Color::Green => formatter.write_str("green"),
            Color::Yellow => formatter.write_str("yellow"),
            Color::Blue => formatter.write_str("blue"),
            Color::Magenta => formatter.write_str("magenta"),
            Color::Cyan => formatter.write_str("cyan"),
            Color::Gray => formatter.write_str("gray"),
            Color::DarkGray => formatter.write_str("dark-gray"),
            Color::LightRed => formatter.write_str("light-red"),
            Color::LightGreen => formatter.write_str("light-green"),
            Color::LightYellow => formatter.write_str("light-yellow"),
            Color::LightBlue => formatter.write_str("light-blue"),
            Color::LightMagenta => formatter.write_str("light-magenta"),
            Color::LightCyan => formatter.write_str("light-cyan"),
            Color::White => formatter.write_str("white"),
            Color::Rgb(red, green, blue) => write!(formatter, "#{red:02X}{green:02X}{blue:02X}"),
            Color::Indexed(index) => write!(formatter, "{index}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ColorScheme, SYSTEM_SCHEME_POLL_INTERVAL, Theme, ThemeMode};
    use nanocodex::Model;
    use ratatui::style::Color;

    #[test]
    fn auto_defaults_to_the_dark_palette_until_the_os_mode_is_known() {
        let theme = Theme::default();

        assert_eq!(theme.mode(), ThemeMode::Auto);
        assert_eq!(theme.text(), Color::Reset);
        assert_eq!(theme.code_text(), Color::Rgb(0xD7, 0xD7, 0xD7));
        assert_eq!(theme.code_background(), Color::Rgb(0x26, 0x26, 0x26));
        assert_eq!(theme.thinking_medium(), Color::Cyan);
    }

    #[test]
    fn system_theme_polling_is_perceptually_immediate() {
        assert!(SYSTEM_SCHEME_POLL_INTERVAL <= std::time::Duration::from_millis(100));
    }

    #[test]
    fn models_have_a_shared_semantic_palette() {
        let theme = Theme::default();

        assert_eq!(theme.model(Model::Luna), Color::White);
        assert_eq!(theme.model(Model::Terra), Color::Green);
        assert_eq!(theme.model(Model::Sol), Color::Yellow);
        assert_eq!(theme.model(Model::Astra), Color::LightMagenta);
    }

    #[test]
    fn auto_tracks_the_system_scheme_while_explicit_modes_do_not() {
        let mut theme = Theme::default();

        assert!(theme.set_system_scheme(ColorScheme::Light));
        assert_eq!(theme.code_text(), Color::Rgb(0x26, 0x26, 0x26));
        assert_eq!(theme.code_background(), Color::Rgb(0xEE, 0xEE, 0xEE));

        theme.set_mode(ThemeMode::Dark);
        assert!(theme.set_system_scheme(ColorScheme::Dark).eq(&false));
        assert_eq!(theme.code_background(), Color::Rgb(0x26, 0x26, 0x26));
    }

    #[test]
    fn shared_and_mode_specific_colors_are_supported() {
        let mut theme: Theme = toml::from_str(
            "mode = \"light\"\naccent = \"#12ABef\"\n[light]\nborder = 238\n[dark]\nborder = 239\n",
        )
        .unwrap();

        assert_eq!(theme.mode(), ThemeMode::Light);
        assert_eq!(theme.accent(), Color::Rgb(0x12, 0xAB, 0xEF));
        assert_eq!(theme.border(), Color::Indexed(238));
        theme.set_mode(ThemeMode::Dark);
        assert_eq!(theme.accent(), Color::Rgb(0x12, 0xAB, 0xEF));
        assert_eq!(theme.border(), Color::Indexed(239));
    }

    #[test]
    fn palettes_serialize_to_ratatui_compatible_strings() {
        let rendered = toml::to_string(&Theme::default()).unwrap();

        assert!(rendered.contains("mode = \"auto\""));
        assert!(rendered.contains("code_text = \"#D7D7D7\""));
        assert!(rendered.contains("code_background = \"#EEEEEE\""));
        assert!(rendered.contains("thinking_max = \"magenta\""));
    }

    #[test]
    fn invalid_colors_are_rejected() {
        let error = toml::from_str::<Theme>("text = \"ultraviolet\"").unwrap_err();

        assert!(error.to_string().contains("invalid terminal color"));
    }
}
