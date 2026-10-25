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
