use crate::config::Config;
use ratatui::style::Color;
use std::sync::{OnceLock, RwLock};

/// Colour roles the launcher draws with. A preset fills them in, and
/// `theme_colors` in `launcher.toml` can override any of them by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub accent: Color,
    pub text: Color,
    pub muted: Color,
    pub ok: Color,
    pub danger: Color,
    pub info: Color,
    pub highlight: Color,
    pub ok_bright: Color,
    pub action: Color,
    pub on_accent: Color,
    pub selection_bg: Color,
    pub selection_bg_alt: Color,
    pub surface: Color,
    pub surface_high: Color,
    pub bar_bg: Color,
    pub row_even: Color,
    pub row_odd: Color,
    pub text_dim: Color,
    pub label_muted: Color,
}

pub const PRESETS: [&str; 3] = ["default", "mono", "warm"];

/// The preset after `current` by `delta` steps, wrapping around.
pub fn cycle(current: &str, delta: isize) -> &'static str {
    let index = PRESETS
        .iter()
        .position(|preset| *preset == current.trim())
        .unwrap_or(0) as isize;
    PRESETS[(index + delta).rem_euclid(PRESETS.len() as isize) as usize]
}

impl Default for Theme {
    fn default() -> Self {
        Self::preset("default").expect("the default preset is built in")
    }
}

impl Theme {
    pub fn preset(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "default" | "" => Some(Self {
                accent: Color::Yellow,
                text: Color::White,
                muted: Color::DarkGray,
                ok: Color::Green,
                danger: Color::Red,
                info: Color::Cyan,
                highlight: Color::Magenta,
                ok_bright: Color::LightGreen,
                action: Color::LightBlue,
                on_accent: Color::Black,
                selection_bg: Color::Rgb(30, 30, 45),
                selection_bg_alt: Color::Rgb(40, 40, 60),
                surface: Color::Rgb(20, 20, 30),
                surface_high: Color::Rgb(25, 25, 40),
                bar_bg: Color::Rgb(15, 15, 25),
                row_even: Color::Rgb(16, 16, 26),
                row_odd: Color::Rgb(22, 22, 34),
                text_dim: Color::Rgb(160, 160, 180),
                label_muted: Color::Rgb(80, 80, 110),
            }),
            "mono" => Some(Self {
                accent: Color::White,
                text: Color::Rgb(220, 220, 220),
                muted: Color::Rgb(130, 130, 130),
                ok: Color::Rgb(200, 200, 200),
                danger: Color::White,
                info: Color::Rgb(160, 160, 160),
                highlight: Color::Rgb(200, 200, 200),
                ok_bright: Color::White,
                action: Color::Rgb(190, 190, 190),
                on_accent: Color::Black,
                selection_bg: Color::Rgb(50, 50, 50),
                selection_bg_alt: Color::Rgb(60, 60, 60),
                surface: Color::Rgb(30, 30, 30),
                surface_high: Color::Rgb(40, 40, 40),
                bar_bg: Color::Rgb(22, 22, 22),
                row_even: Color::Rgb(28, 28, 28),
                row_odd: Color::Rgb(36, 36, 36),
                text_dim: Color::Rgb(150, 150, 150),
                label_muted: Color::Rgb(120, 120, 120),
            }),
            "warm" => Some(Self {
                accent: Color::Rgb(215, 175, 115),
                text: Color::Rgb(230, 225, 215),
                muted: Color::Rgb(120, 110, 100),
                ok: Color::Rgb(150, 190, 120),
                danger: Color::Rgb(220, 100, 90),
                info: Color::Rgb(190, 140, 80),
                highlight: Color::Rgb(210, 130, 110),
                ok_bright: Color::Rgb(170, 210, 140),
                action: Color::Rgb(210, 170, 140),
                on_accent: Color::Rgb(40, 30, 20),
                selection_bg: Color::Rgb(60, 45, 35),
                selection_bg_alt: Color::Rgb(75, 55, 40),
                surface: Color::Rgb(35, 30, 25),
                surface_high: Color::Rgb(45, 38, 30),
                bar_bg: Color::Rgb(28, 24, 20),
                row_even: Color::Rgb(32, 27, 22),
                row_odd: Color::Rgb(42, 35, 28),
                text_dim: Color::Rgb(170, 155, 140),
                label_muted: Color::Rgb(140, 120, 100),
            }),
            _ => None,
        }
    }

    /// The named preset from the config, with any `theme_colors` override applied.
    /// Unknown preset names fall back to `default`.
    pub fn from_config(config: &Config) -> Self {
        let mut theme = Self::preset(&config.theme).unwrap_or_default();
        for (role, value) in &config.theme_colors {
            let Some(color) = parse_color(value) else {
                continue;
            };
            if !theme.set(role, color) {
                continue;
            }
        }
        theme
    }

    fn set(&mut self, role: &str, color: Color) -> bool {
        match role.trim().to_ascii_lowercase().as_str() {
            "accent" => self.accent = color,
            "text" => self.text = color,
            "muted" => self.muted = color,
            "ok" => self.ok = color,
            "danger" => self.danger = color,
            "info" => self.info = color,
            "highlight" => self.highlight = color,
            "ok_bright" => self.ok_bright = color,
            "action" => self.action = color,
            "on_accent" => self.on_accent = color,
            "selection_bg" => self.selection_bg = color,
            "selection_bg_alt" => self.selection_bg_alt = color,
            "surface" => self.surface = color,
            "surface_high" => self.surface_high = color,
            "bar_bg" => self.bar_bg = color,
            "row_even" => self.row_even = color,
            "row_odd" => self.row_odd = color,
            "text_dim" => self.text_dim = color,
            "label_muted" => self.label_muted = color,
            _ => return false,
        }
        true
    }
}

/// `#rrggbb`, `#rgb`, or a ratatui colour name.
fn parse_color(value: &str) -> Option<Color> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        let expanded: String = match hex.len() {
            3 => hex.chars().flat_map(|c| [c, c]).collect(),
            6 => hex.to_string(),
            _ => return None,
        };
        let r = u8::from_str_radix(&expanded[0..2], 16).ok()?;
        let g = u8::from_str_radix(&expanded[2..4], 16).ok()?;
        let b = u8::from_str_radix(&expanded[4..6], 16).ok()?;
        return Some(Color::Rgb(r, g, b));
    }

    match value
        .to_ascii_lowercase()
        .replace([' ', '-', '_'], "")
        .as_str()
    {
        "black" => Some(Color::Black),
        "white" => Some(Color::White),
        "gray" | "grey" => Some(Color::Gray),
        "darkgray" | "darkgrey" => Some(Color::DarkGray),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        _ => None,
    }
}

static THEME: OnceLock<RwLock<Theme>> = OnceLock::new();

fn lock() -> &'static RwLock<Theme> {
    THEME.get_or_init(|| RwLock::new(Theme::default()))
}

pub fn set(theme: Theme) {
    if let Ok(mut guard) = lock().write() {
        *guard = theme;
    }
}

pub fn current() -> Theme {
    lock().read().map(|guard| *guard).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn presets_resolve_by_name_and_fall_back() {
        assert_eq!(Theme::preset("warm"), Theme::preset("  WARM "));
        assert!(Theme::preset("nonsense").is_none());
    }

    #[test]
    fn hex_overrides_replace_a_role() {
        let config = Config {
            theme: "mono".to_string(),
            theme_colors: BTreeMap::from([("accent".to_string(), "#102030".to_string())]),
            ..Config::default()
        };
        let theme = Theme::from_config(&config);
        assert_eq!(theme.accent, Color::Rgb(0x10, 0x20, 0x30));
        assert_eq!(theme.text, Theme::preset("mono").unwrap().text);
    }

    #[test]
    fn bad_overrides_are_ignored() {
        let config = Config {
            theme: "default".to_string(),
            theme_colors: BTreeMap::from([
                ("accent".to_string(), "not-a-colour".to_string()),
                ("no_such_role".to_string(), "#ffffff".to_string()),
            ]),
            ..Config::default()
        };
        assert_eq!(Theme::from_config(&config), Theme::default());
    }

    #[test]
    fn short_hex_is_expanded() {
        assert_eq!(parse_color("#abc"), Some(Color::Rgb(0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_color("dark gray"), Some(Color::DarkGray));
        assert_eq!(parse_color("#12345"), None);
    }
}
