use ratatui::style::{Color, Modifier, Style};

pub const BG: Color = Color::Rgb(0x0b, 0x17, 0x10);
pub const FG: Color = Color::Rgb(0xd6, 0xe8, 0xda);
pub const GREEN: Color = Color::Rgb(0x7e, 0xe2, 0x9a);
pub const GREEN_DIM: Color = Color::Rgb(0x3f, 0x8f, 0x57);
pub const BORDER: Color = Color::Rgb(0x4f, 0xb8, 0x6c);
pub const MUTED: Color = Color::Rgb(0x8a, 0xa3, 0x90);
pub const ERROR: Color = Color::Rgb(0xff, 0x7a, 0x7a);
pub const BADGE: Color = Color::Rgb(0xff, 0xd8, 0x6b);

pub fn base() -> Style {
    Style::new().fg(FG).bg(BG)
}
pub fn border(focused: bool) -> Style {
    Style::new().fg(if focused { GREEN } else { BORDER })
}
pub fn heading() -> Style {
    Style::new().fg(GREEN).add_modifier(Modifier::BOLD)
}
pub fn accent() -> Style {
    Style::new().fg(GREEN)
}
pub fn muted() -> Style {
    Style::new().fg(MUTED)
}
pub fn selected(focused: bool) -> Style {
    if focused {
        Style::new().fg(BG).bg(GREEN).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(GREEN).add_modifier(Modifier::BOLD)
    }
}
