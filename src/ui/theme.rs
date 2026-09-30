use ratatui::style::{Color, Modifier, Style};

/// Names of the built-in themes, in the order `t` cycles through them.
pub const PRESETS: &[&str] = &["green", "blue", "purple", "amber", "red", "mono"];

/// The interface colors. Text uses the terminal's own color and there is no background
/// unless the config sets one, so the app fits light and dark terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub text: Color,
    pub accent: Color,
    /// Quieter accent: the empty part of the progress bar and low spectrum bars.
    pub dim: Color,
    pub border: Color,
    pub muted: Color,
    pub error: Color,
    pub badge: Color,
    /// Text on the accent-colored selection bar.
    pub selection_text: Color,
    pub background: Option<Color>,
}

impl Theme {
    /// A built-in theme by name.
    pub fn preset(name: &str) -> Option<Theme> {
        let theme = |accent, dim, border, muted, selection_text| Theme {
            text: Color::Reset,
            accent,
            dim,
            border,
            muted,
            error: Color::Rgb(0xff, 0x7a, 0x7a),
            badge: Color::Rgb(0xff, 0xd8, 0x6b),
            selection_text,
            background: None,
        };
        Some(match name {
            "green" => theme(
                Color::Rgb(0x7e, 0xe2, 0x9a),
                Color::Rgb(0x3f, 0x8f, 0x57),
                Color::Rgb(0x4f, 0xb8, 0x6c),
                Color::Rgb(0x8a, 0xa3, 0x90),
                Color::Rgb(0x0b, 0x17, 0x10),
            ),
            "blue" => theme(
                Color::Rgb(0x7a, 0xb8, 0xff),
                Color::Rgb(0x3d, 0x6a, 0x9e),
                Color::Rgb(0x5a, 0x9b, 0xe0),
                Color::Rgb(0x8a, 0x9b, 0xb0),
                Color::Rgb(0x0a, 0x12, 0x20),
            ),
            "purple" => theme(
                Color::Rgb(0xc4, 0x9b, 0xff),
                Color::Rgb(0x6e, 0x4f, 0xa3),
                Color::Rgb(0xa6, 0x7e, 0xe8),
                Color::Rgb(0xa1, 0x97, 0xb3),
                Color::Rgb(0x15, 0x0d, 0x22),
            ),
            "amber" => theme(
                Color::Rgb(0xff, 0xb4, 0x54),
                Color::Rgb(0x9a, 0x6a, 0x2a),
                Color::Rgb(0xe0, 0x9a, 0x3e),
                Color::Rgb(0xb3, 0xa4, 0x8a),
                Color::Rgb(0x1f, 0x14, 0x05),
            ),
            "red" => theme(
                Color::Rgb(0xff, 0x6b, 0x81),
                Color::Rgb(0x9e, 0x3a, 0x4a),
                Color::Rgb(0xe0, 0x55, 0x68),
                Color::Rgb(0xb3, 0x9a, 0x9e),
                Color::Rgb(0x22, 0x0a, 0x0e),
            ),
            // The terminal's own palette, so it follows the terminal's color scheme.
            "mono" => Theme {
                error: Color::LightRed,
                badge: Color::Yellow,
                ..theme(
                    Color::White,
                    Color::DarkGray,
                    Color::Gray,
                    Color::DarkGray,
                    Color::Black,
                )
            },
            _ => return None,
        })
    }

    /// The theme named in the config with its `[colors]` applied, and a warning for each
    /// name or color that couldn't be used.
    pub fn from_config(name: &str, colors: &crate::config::ColorOverrides) -> (Theme, Vec<String>) {
        let mut warnings = Vec::new();
        let mut theme = Theme::preset(name).unwrap_or_else(|| {
            warnings.push(format!(
                "Unknown theme \"{name}\" in config.toml; using green ({})",
                PRESETS.join(", ")
            ));
            Theme::preset("green").expect("built in")
        });
        let mut apply = |key: &str, value: &Option<String>, slot: &mut Color| {
            if let Some(text) = value {
                match parse_color(text) {
                    Some(color) => *slot = color,
                    None => warnings.push(format!(
                        "Unknown color \"{text}\" for {key} in config.toml; use \"#rrggbb\", a name or 0-255"
                    )),
                }
            }
        };
        apply("text", &colors.text, &mut theme.text);
        apply("accent", &colors.accent, &mut theme.accent);
        apply("dim", &colors.dim, &mut theme.dim);
        apply("border", &colors.border, &mut theme.border);
        apply("muted", &colors.muted, &mut theme.muted);
        apply("error", &colors.error, &mut theme.error);
        apply("badge", &colors.badge, &mut theme.badge);
        apply(
            "selection_text",
            &colors.selection_text,
            &mut theme.selection_text,
        );
        let mut background = Color::Reset;
        apply("background", &colors.background, &mut background);
        if background != Color::Reset {
            theme.background = Some(background);
        }
        (theme, warnings)
    }

    /// Plain text, with the background when the theme has one.
    pub fn base(&self) -> Style {
        let style = Style::new().fg(self.text);
        match self.background {
            Some(bg) => style.bg(bg),
            None => style,
        }
    }
    pub fn border(&self, focused: bool) -> Style {
        Style::new().fg(if focused { self.accent } else { self.border })
    }
    pub fn heading(&self) -> Style {
        self.accent().add_modifier(Modifier::BOLD)
    }
    pub fn accent(&self) -> Style {
        Style::new().fg(self.accent)
    }
    pub fn muted(&self) -> Style {
        Style::new().fg(self.muted)
    }
    pub fn error(&self) -> Style {
        Style::new().fg(self.error)
    }
    pub fn badge(&self) -> Style {
        Style::new().fg(self.badge)
    }
    /// The selected row: a solid accent bar in the focused pane, accent text elsewhere.
    pub fn selected(&self, focused: bool) -> Style {
        if focused {
            Style::new()
                .fg(self.selection_text)
                .bg(self.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            self.heading()
        }
    }
}

/// The built-in theme after `current`, wrapping around.
pub fn next_preset(current: &str) -> &'static str {
    match PRESETS.iter().position(|p| *p == current) {
        Some(i) => PRESETS[(i + 1) % PRESETS.len()],
        None => PRESETS[0],
    }
}

/// A color written as `"#7ee29a"`, a name such as `"cyan"`, or a number from 0 to 255.
pub fn parse_color(text: &str) -> Option<Color> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix('#') {
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        return Some(Color::Rgb(channel(0)?, channel(2)?, channel(4)?));
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorOverrides;

    #[test]
    fn colors_can_be_hex_names_or_numbers() {
        assert_eq!(parse_color("#7ee29a"), Some(Color::Rgb(0x7e, 0xe2, 0x9a)));
        assert_eq!(parse_color(" Cyan "), Some(Color::Cyan));
        assert_eq!(parse_color("light-red"), Some(Color::LightRed));
        assert_eq!(parse_color("208"), Some(Color::Indexed(208)));
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("sparkly"), None);
        assert_eq!(parse_color(""), None);
    }

    #[test]
    fn every_preset_exists_and_leaves_the_background_alone() {
        for name in PRESETS {
            let theme = Theme::preset(name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(theme.background, None, "{name}");
            assert_eq!(theme.text, Color::Reset, "{name}");
        }
        assert_eq!(Theme::preset("plaid"), None);
        assert_eq!(
            Theme::preset("green").unwrap().accent,
            Color::Rgb(0x7e, 0xe2, 0x9a)
        );
    }

    #[test]
    fn next_preset_cycles_and_starts_over() {
        assert_eq!(next_preset("green"), "blue");
        assert_eq!(next_preset("mono"), "green");
        assert_eq!(next_preset("plaid"), "green");
    }

    #[test]
    fn config_colors_replace_the_theme_colors() {
        let colors = ColorOverrides {
            accent: Some("#ff0000".into()),
            background: Some("black".into()),
            ..ColorOverrides::default()
        };
        let (theme, warnings) = Theme::from_config("blue", &colors);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(theme.accent, Color::Rgb(0xff, 0, 0));
        assert_eq!(theme.background, Some(Color::Black));
        assert_eq!(theme.border, Theme::preset("blue").unwrap().border);
    }

    #[test]
    fn bad_names_and_colors_fall_back_with_a_warning() {
        let colors = ColorOverrides {
            accent: Some("sparkly".into()),
            ..ColorOverrides::default()
        };
        let (theme, warnings) = Theme::from_config("plaid", &colors);
        assert_eq!(theme, Theme::preset("green").unwrap());
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("plaid"), "{warnings:?}");
        assert!(warnings[1].contains("accent"), "{warnings:?}");
    }

    #[test]
    fn styles_only_set_a_background_when_the_theme_has_one() {
        let mut theme = Theme::preset("green").unwrap();
        assert_eq!(theme.base().bg, None);
        theme.background = Some(Color::Black);
        assert_eq!(theme.base().bg, Some(Color::Black));
        let selected = theme.selected(true);
        assert_eq!(
            (selected.fg, selected.bg),
            (Some(theme.selection_text), Some(theme.accent))
        );
    }
}
