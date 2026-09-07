//! Server casemapping.
//!
//! Two nicks or two channel names are the same identity when they fold to the same string under the
//! server's `CASEMAPPING`. Under `rfc1459` that means `[]\~` fold together with `{}|^`, because the
//! original protocol treated them as one alphabet. An ASCII `to_lowercase` gets this wrong on every
//! server that does not advertise `ascii`, which is most of them.

use alloc::string::String;
use core::fmt;

/// How a server folds case, from the `CASEMAPPING` ISUPPORT token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub enum Casemapping {
    /// `A-Z` only.
    Ascii,
    /// `A-Z` plus `[]\~` folding onto `{}|^`. The default when a server advertises nothing.
    #[default]
    Rfc1459,
    /// `rfc1459` without the `~` to `^` fold.
    Rfc1459Strict,
}

impl Casemapping {
    /// Read the value of a `CASEMAPPING` token. An unknown value falls back to `rfc1459`, which is
    /// what a server that advertises nothing is assumed to use.
    pub fn parse(token: &str) -> Self {
        match token {
            "ascii" => Self::Ascii,
            "rfc1459-strict" => Self::Rfc1459Strict,
            _ => Self::Rfc1459,
        }
    }

    /// Fold one character.
    pub fn fold_char(self, c: char) -> char {
        match c {
            'A'..='Z' => c.to_ascii_lowercase(),
            '[' | ']' | '\\' if self != Self::Ascii => match c {
                '[' => '{',
                ']' => '}',
                _ => '|',
            },
            '~' if self == Self::Rfc1459 => '^',
            _ => c,
        }
    }

    /// Fold a whole name into a key that can be compared and hashed.
    pub fn fold(self, name: &str) -> CaseFolded {
        CaseFolded(name.chars().map(|c| self.fold_char(c)).collect())
    }

    /// Whether two names are the same identity under this mapping, without allocating.
    pub fn eq(self, a: &str, b: &str) -> bool {
        let mut a = a.chars();
        let mut b = b.chars();
        loop {
            match (a.next(), b.next()) {
                (None, None) => return true,
                (Some(x), Some(y)) if self.fold_char(x) == self.fold_char(y) => {}
                _ => return false,
            }
        }
    }
}

/// A nick or channel name folded under a server's casemapping.
///
/// Every map keyed by a nick or a channel is keyed by this type, so that a raw `String` can never be
/// used as an identity by accident.
///
/// The mapping that produced it is not stored. Two `CaseFolded` values are only comparable when they
/// came from the same connection, which is the only place they are ever used together.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[derive(Default)]
pub struct CaseFolded(String);

impl CaseFolded {
    /// The folded form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Take the folded string.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for CaseFolded {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CaseFolded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::borrow::Borrow<str> for CaseFolded {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<CaseFolded> for String {
    fn from(folded: CaseFolded) -> Self {
        folded.0
    }
}

/// Fold with the default mapping, for the window before ISUPPORT arrives.
impl From<&str> for CaseFolded {
    fn from(name: &str) -> Self {
        Casemapping::default().fold(name)
    }
}

impl CaseFolded {
    /// Wrap a string that is already folded, such as one read back from storage.
    pub fn already_folded(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_folds_only_letters() {
        let map = Casemapping::Ascii;
        assert_eq!(map.fold("NiCk").as_str(), "nick");
        assert_eq!(map.fold("[a]").as_str(), "[a]");
    }

    #[test]
    fn rfc1459_folds_the_bracket_alphabet() {
        let map = Casemapping::Rfc1459;
        assert_eq!(map.fold(r"[Nick]\~").as_str(), "{nick}|^");
    }

    #[test]
    fn strict_leaves_tilde_alone() {
        assert_eq!(Casemapping::Rfc1459Strict.fold("~a").as_str(), "~a");
        assert_eq!(Casemapping::Rfc1459.fold("~a").as_str(), "^a");
    }

    #[test]
    fn eq_matches_fold() {
        let map = Casemapping::Rfc1459;
        assert!(map.eq("[nick]", "{NICK}"));
        assert!(!map.eq("nick", "nick2"));
        assert!(!Casemapping::Ascii.eq("[nick]", "{nick}"));
    }

    #[test]
    fn parses_the_isupport_token() {
        assert_eq!(Casemapping::parse("ascii"), Casemapping::Ascii);
        assert_eq!(
            Casemapping::parse("rfc1459-strict"),
            Casemapping::Rfc1459Strict
        );
        assert_eq!(Casemapping::parse("rfc1459"), Casemapping::Rfc1459);
        assert_eq!(Casemapping::parse("something-else"), Casemapping::Rfc1459);
    }
}
