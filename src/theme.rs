//! The user's colour-scheme preference.
//!
//! Rendered as `data-theme` on `<html>`, which `assets/tokens.css` keys all three of its
//! token blocks off. The server always emits one of these three values so the CSS can use
//! positive selectors only.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Follow the operating system. The default, and what pre-authentication pages use
    /// since they have no user row to read.
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// Parses the stored column value. The database has a CHECK constraint on it, so an
    /// unrecognised value means someone edited the row by hand; following the OS is the
    /// safe reading of that.
    pub fn from_db(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_stored_representation() {
        for theme in [Theme::System, Theme::Light, Theme::Dark] {
            assert_eq!(Theme::from_db(theme.as_str()), theme);
        }
    }

    #[test]
    fn defaults_to_following_the_os() {
        assert_eq!(Theme::default(), Theme::System);
        assert_eq!(Theme::from_db("neon"), Theme::System);
        assert_eq!(Theme::from_db(""), Theme::System);
    }
}
