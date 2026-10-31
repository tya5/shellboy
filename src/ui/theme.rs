//! Display-only palette shared by UI drawing and worker-generated Markdown styles.
use ratatui::style::Color;

// The terminal foreground remains readable on both light and dark backgrounds.
// Hierarchy uses labels, spacing and markers; muted text must remain readable.
pub(crate) const TEXT: Color = Color::Reset;
pub(crate) const MUTED: Color = Color::Reset;
pub(crate) const EMPHASIS: Color = Color::Reset;
pub(crate) const DEFAULT_BACKGROUND: Color = Color::Reset;

pub(crate) const ACCENT: Color = Color::Cyan;
pub(crate) const ATTENTION: Color = Color::LightRed;
pub(crate) const WARNING: Color = Color::LightYellow;
pub(crate) const SUCCESS: Color = Color::LightGreen;
pub(crate) const SECTION_HEADING: Color = Color::LightCyan;
pub(crate) const GROUP_HEADING: Color = Color::LightBlue;
pub(crate) const REPLACEMENT: Color = Color::LightBlue;
pub(crate) const TOOL: Color = Color::Cyan;
pub(crate) const USER_MARKER: Color = Color::Cyan;
pub(crate) const ASSISTANT_MARKER: Color = Color::LightBlue;
pub(crate) const LINK: Color = Color::LightBlue;
// Inline links use the terminal foreground and underline; explicit URL rows use LINK.
pub(crate) const MARKDOWN_LINK: Color = Color::Reset;
pub(crate) const SELECTION_FOREGROUND: Color = Color::Cyan;
pub(crate) const SELECTION_BACKGROUND: Color = Color::Rgb(32, 44, 58);

// These are a paired code surface, independent of the terminal background.
// Syntax highlighting remains in the OSS Markdown renderer's bundled theme.
pub(crate) const CODE_LABEL: Color = Color::Cyan;
pub(crate) const CODE_TEXT: Color = Color::Rgb(208, 214, 223);
pub(crate) const CODE_BACKGROUND: Color = Color::Rgb(30, 34, 40);
pub(crate) const CODE_CAPTION: Color = Color::Rgb(137, 151, 169);
pub(crate) const ACTIVITY_HIGHLIGHT: Color = Color::LightCyan;
pub(crate) fn activity_pulse(level: f64) -> Color {
    Color::Rgb(
        (65.0 + 95.0 * level) as u8,
        (125.0 + 105.0 * level) as u8,
        (150.0 + 95.0 * level) as u8,
    )
}

/// Immutable process-local capabilities; no terminal probing or runtime I/O.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Palette {
    choice: crate::config::PaletteMode,
    no_color: bool,
    rich: bool,
}
impl Palette {
    pub(crate) fn detect(choice: crate::config::PaletteMode) -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        let term = std::env::var("TERM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let color = std::env::var("COLORTERM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let rich = term != "dumb"
            && (term.contains("256color") || matches!(color.as_str(), "truecolor" | "24bit"));
        Self::from_capabilities(choice, no_color, rich)
    }
    pub(crate) fn from_capabilities(
        choice: crate::config::PaletteMode,
        no_color: bool,
        rich: bool,
    ) -> Self {
        Self {
            choice,
            no_color,
            rich,
        }
    }
    pub(crate) fn plain(self) -> bool {
        self.no_color
    }
    pub(crate) fn muted(self) -> Color {
        if self.no_color || !self.rich {
            return Color::Reset;
        }
        match self.choice {
            crate::config::PaletteMode::Dark => Color::Indexed(245),
            crate::config::PaletteMode::Light => Color::Indexed(239),
        }
    }
    pub(crate) fn header(self) -> ratatui::style::Style {
        use ratatui::style::{Modifier, Style};
        if self.no_color || !self.rich {
            return Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
        match self.choice {
            crate::config::PaletteMode::Dark => Style::default()
                .fg(Color::Indexed(252))
                .bg(Color::Indexed(234)),
            crate::config::PaletteMode::Light => Style::default()
                .fg(Color::Indexed(234))
                .bg(Color::Indexed(252)),
        }
    }
    pub(crate) fn user_band(
        self,
        source: crate::presentation::InputSource,
    ) -> ratatui::style::Style {
        use ratatui::style::{Modifier, Style};
        if self.no_color {
            return Style::default();
        }
        if !self.rich {
            return Style::default().add_modifier(Modifier::REVERSED);
        }
        let human = source == crate::presentation::InputSource::Human;
        match self.choice {
            crate::config::PaletteMode::Dark => Style::default()
                .fg(Color::Indexed(252))
                .bg(Color::Indexed(if human { 236 } else { 235 })),
            crate::config::PaletteMode::Light => Style::default()
                .fg(Color::Indexed(234))
                .bg(Color::Indexed(if human { 254 } else { 253 })),
        }
    }
    pub(crate) fn history_focus(self) -> ratatui::style::Style {
        use ratatui::style::{Modifier, Style};
        if self.no_color || !self.rich {
            return Style::default().add_modifier(Modifier::BOLD);
        }
        Style::default().bg(match self.choice {
            crate::config::PaletteMode::Dark => Color::Indexed(238),
            crate::config::PaletteMode::Light => Color::Indexed(251),
        })
    }
    pub(crate) fn interactive(self) -> ratatui::style::Style {
        use ratatui::style::{Modifier, Style};
        let style = Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        if self.no_color || !self.rich {
            style
        } else {
            style.fg(match self.choice {
                crate::config::PaletteMode::Dark => Color::Indexed(45),
                crate::config::PaletteMode::Light => Color::Indexed(24),
            })
        }
    }
}

#[cfg(test)]
mod palette_tests {
    use super::*;
    use crate::{
        config::{Config, PaletteMode},
        presentation::InputSource,
    };
    use ratatui::style::{Modifier, Style};
    #[test]
    fn dark_light_surfaces_and_plain_reverse_fallback_are_explicit() {
        let dark = Palette::from_capabilities(PaletteMode::Dark, false, true);
        let light = Palette::from_capabilities(PaletteMode::Light, false, true);
        assert_ne!(dark.header().bg, dark.user_band(InputSource::Human).bg);
        assert_ne!(light.header().bg, light.user_band(InputSource::Human).bg);
        assert_ne!(
            dark.user_band(InputSource::Human).bg,
            light.user_band(InputSource::Human).bg
        );
        assert_ne!(
            dark.user_band(InputSource::Human).bg,
            dark.user_band(InputSource::Parent).bg
        );
        let reverse = Palette::from_capabilities(PaletteMode::Dark, false, false);
        assert!(
            reverse
                .user_band(InputSource::Human)
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert_ne!(reverse.header(), reverse.user_band(InputSource::Human));
        for mode in [PaletteMode::Dark, PaletteMode::Light] {
            let plain = Palette::from_capabilities(mode, true, true);
            assert!(plain.plain());
            assert_eq!(plain.user_band(InputSource::Human), Style::default());
            assert_eq!(plain.muted(), Color::Reset);
            assert!(plain.header().fg.is_none() && plain.header().bg.is_none());
            assert!(plain.interactive().fg.is_none() && plain.interactive().bg.is_none());
        }
    }
    #[test]
    fn palette_config_defaults_and_strict_variants_match() {
        assert_eq!(Config::default().ui.palette, PaletteMode::Dark);
        assert_eq!(
            Config::parse("[ui]\npalette='light'").unwrap().ui.palette,
            PaletteMode::Light
        );
        assert!(Config::parse("[ui]\npalette='auto'").is_err());
        assert!(Config::parse("[ui]\npalette=true").is_err());
    }
}
