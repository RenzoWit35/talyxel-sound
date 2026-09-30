/// The "TS" emblem.
pub const LOGO: &[&str] = &[
    r"__/\\\\\\\\\\\\\\\_____/\\\\\\\\\\\___        ",
    r" _\///////\\\/////____/\\\/////////\\\_       ",
    r"  _______\/\\\________\//\\\______\///__      ",
    r"   _______\/\\\_________\////\\\_________     ",
    r"    _______\/\\\____________\////\\\______    ",
    r"     _______\/\\\_______________\////\\\___   ",
    r"      _______\/\\\________/\\\______\//\\\__  ",
    r"       _______\/\\\_______\///\\\\\\\\\\\/___ ",
    r"        _______\///__________\///////////_____",
];

pub const WORDMARK: &str = "T A L Y X E L  S O U N D";

pub const HEIGHT: u16 = LOGO.len() as u16 + 1;
pub const WIDTH: u16 = 46;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_is_the_ts_emblem() {
        assert_eq!(LOGO.len(), 9);
        assert!(LOGO[0].starts_with(r"__/\\\\\\\\\\\\\\\_____/\\\\\\\\\\\___"));
        assert!(LOGO[8].ends_with(r"_______\///__________\///////////_____"));
        for line in LOGO {
            assert_eq!(line.chars().count(), WIDTH as usize, "{line:?}");
        }
    }
}
