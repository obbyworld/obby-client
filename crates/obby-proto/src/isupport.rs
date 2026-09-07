//! The `005` ISUPPORT tokens, and the typed settings they drive.
//!
//! Everything a client does with names and modes depends on these: which characters start a channel,
//! which modes take an argument, which prefix outranks which. A client that assumes defaults instead
//! of reading them is wrong on any server that is not the one it was written against.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::casemap::{CaseFolded, Casemapping};

/// The membership prefixes a server uses, in rank order, highest first.
///
/// Parsed from `PREFIX=(ov)@+`, which pairs each mode letter with the character that shows it in a
/// NAMES reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefix {
    modes: Vec<char>,
    chars: Vec<char>,
}

impl Default for Prefix {
    fn default() -> Self {
        Self {
            modes: alloc::vec!['o', 'v'],
            chars: alloc::vec!['@', '+'],
        }
    }
}

impl Prefix {
    /// Parse a `PREFIX` value. A malformed value leaves the default in place, because guessing here
    /// silently corrupts every member list.
    pub fn parse(value: &str) -> Option<Self> {
        let inner = value.strip_prefix('(')?;
        let (modes, chars) = inner.split_once(')')?;
        if modes.is_empty() || modes.chars().count() != chars.chars().count() {
            return None;
        }
        Some(Self {
            modes: modes.chars().collect(),
            chars: chars.chars().collect(),
        })
    }

    /// The prefix character a mode letter grants, such as `o` giving `@`.
    pub fn char_for_mode(&self, mode: char) -> Option<char> {
        let index = self.modes.iter().position(|m| *m == mode)?;
        self.chars.get(index).copied()
    }

    /// The mode letter a prefix character stands for, such as `@` meaning `o`.
    pub fn mode_for_char(&self, prefix: char) -> Option<char> {
        let index = self.chars.iter().position(|c| *c == prefix)?;
        self.modes.get(index).copied()
    }

    /// True when this mode letter grants membership status rather than setting a channel mode.
    pub fn is_membership_mode(&self, mode: char) -> bool {
        self.modes.contains(&mode)
    }

    /// How highly a prefix ranks, counting from zero for the highest. Used to sort a member list.
    pub fn rank(&self, prefix: char) -> Option<usize> {
        self.chars.iter().position(|c| *c == prefix)
    }

    /// Split the leading prefixes off a NAMES entry, giving `("@+", "nick")`.
    ///
    /// With `multi-prefix` an entry carries every prefix the member holds; without it, only the
    /// highest. Both split the same way.
    pub fn split<'a>(&self, entry: &'a str) -> (&'a str, &'a str) {
        let end = entry
            .char_indices()
            .find(|(_, c)| !self.chars.contains(c))
            .map_or(entry.len(), |(i, _)| i);
        entry.split_at(end)
    }
}

/// The four classes of channel mode, from `CHANMODES=beI,k,l,imnpst`.
///
/// The class decides whether a mode takes an argument, which is the only way to parse a MODE line at
/// all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChanModes {
    /// List modes such as bans. Always take an argument, setting and unsetting.
    pub list: String,
    /// Modes that always take an argument, such as a key.
    pub always_arg: String,
    /// Modes that take an argument only when set, such as a user limit.
    pub arg_on_set: String,
    /// Flags that never take an argument.
    pub flag: String,
}

impl Default for ChanModes {
    fn default() -> Self {
        Self {
            list: "b".to_owned(),
            always_arg: "k".to_owned(),
            arg_on_set: "l".to_owned(),
            flag: "imnpst".to_owned(),
        }
    }
}

impl ChanModes {
    /// Parse a `CHANMODES` value. Classes past the fourth are ignored, as the specification says to
    /// treat them as flags we do not know about.
    pub fn parse(value: &str) -> Self {
        let mut parts = value.split(',');
        Self {
            list: parts.next().unwrap_or_default().to_owned(),
            always_arg: parts.next().unwrap_or_default().to_owned(),
            arg_on_set: parts.next().unwrap_or_default().to_owned(),
            flag: parts.next().unwrap_or_default().to_owned(),
        }
    }
}

/// Everything the server told us about itself in `005`.
#[derive(Debug, Clone, Default)]
pub struct Isupport {
    tokens: BTreeMap<String, Option<String>>,
    casemapping: Casemapping,
    prefix: Prefix,
    chanmodes: ChanModes,
    chantypes: String,
    statusmsg: String,
}

/// One token after the wire grammar has been read off it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Token {
    /// The token name, with any `-` prefix or `+` append marker already removed.
    pub name: String,
    /// The value now in force, which for an append is the whole accumulated value.
    pub value: Option<String>,
    /// True when the server withdrew the token.
    pub removed: bool,
}

impl Isupport {
    /// Apply one token.
    ///
    /// A leading `-` removes the token and restores the default. A `+=` in place of `=` appends to
    /// the value already held, which is how `draft/extended-isupport-0.2` carries a value too long
    /// for one line. The typed views always see the cumulative value, never the fragment.
    ///
    /// Returns what the token turned out to mean, so a caller never has to read the wire grammar a
    /// second time to find out.
    pub fn apply(&mut self, token: &str) -> Token {
        if let Some(name) = token.strip_prefix('-') {
            let name = name.split('=').next().unwrap_or(name);
            self.tokens.remove(name);
            self.recompute(name, None);
            return Token {
                name: name.to_owned(),
                value: None,
                removed: true,
            };
        }
        let Some((name, raw)) = token.split_once('=') else {
            self.tokens.insert(token.to_owned(), None);
            self.recompute(token, None);
            return Token {
                name: token.to_owned(),
                value: None,
                removed: false,
            };
        };
        let (name, value) = match name.strip_suffix('+') {
            Some(name) => {
                let mut joined = self.tokens.get(name).cloned().flatten().unwrap_or_default();
                joined.push_str(&unescape(raw));
                (name, joined)
            }
            None => (name, unescape(raw)),
        };
        self.tokens.insert(name.to_owned(), Some(value.clone()));
        self.recompute(name, Some(&value));
        Token {
            name: name.to_owned(),
            value: Some(value),
            removed: false,
        }
    }

    fn recompute(&mut self, name: &str, value: Option<&str>) {
        match name {
            "CASEMAPPING" => {
                self.casemapping = value.map_or_else(Casemapping::default, Casemapping::parse);
            }
            // a malformed PREFIX keeps the previous value rather than clearing the member list
            "PREFIX" => {
                self.prefix = value.and_then(Prefix::parse).unwrap_or_else(|| {
                    if value.is_none() {
                        Prefix::default()
                    } else {
                        self.prefix.clone()
                    }
                });
            }
            "CHANMODES" => {
                self.chanmodes = value.map_or_else(ChanModes::default, ChanModes::parse);
            }
            "CHANTYPES" => value.unwrap_or_default().clone_into(&mut self.chantypes),
            "STATUSMSG" => value.unwrap_or_default().clone_into(&mut self.statusmsg),
            _ => {}
        }
    }

    /// The raw value of a token, `None` if the token is absent, `Some(None)` if it is a bare flag.
    pub fn get(&self, name: &str) -> Option<Option<&str>> {
        self.tokens.get(name).map(Option::as_deref)
    }

    /// True when the server advertised this token at all.
    pub fn has(&self, name: &str) -> bool {
        self.tokens.contains_key(name)
    }

    /// A token whose value is a number, such as `LINELEN` or `MONITOR`.
    pub fn number(&self, name: &str) -> Option<u32> {
        self.get(name)?.and_then(|value| value.parse().ok())
    }

    /// How the server folds case.
    pub fn casemapping(&self) -> Casemapping {
        self.casemapping
    }

    /// Fold a nick or channel name into a key.
    pub fn fold(&self, name: &str) -> CaseFolded {
        self.casemapping.fold(name)
    }

    /// The membership prefixes.
    pub fn prefix(&self) -> &Prefix {
        &self.prefix
    }

    /// The channel mode classes.
    pub fn chanmodes(&self) -> &ChanModes {
        &self.chanmodes
    }

    /// True when this name starts with a character the server treats as a channel prefix.
    ///
    /// `CHANTYPES` defaults to `#&` when absent, and Obby adds `^` for voice channels and `$` for
    /// stream channels, which arrive in this token like any other.
    pub fn is_channel(&self, name: &str) -> bool {
        let types = if self.chantypes.is_empty() {
            "#&"
        } else {
            &self.chantypes
        };
        name.chars().next().is_some_and(|c| types.contains(c))
    }

    /// True when this character targets a channel subset, as in `@#channel`.
    pub fn is_statusmsg(&self, c: char) -> bool {
        self.statusmsg.contains(c)
    }

    /// The most messages of one command that may share a line, from `TARGMAX`.
    pub fn targmax(&self, command: &str) -> Option<u32> {
        let value = self.get("TARGMAX")??;
        value
            .split(',')
            .filter_map(|pair| pair.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case(command))
            .and_then(|(_, limit)| limit.parse().ok())
    }
}

/// Undo the `\xHH` escaping an ISUPPORT value may carry.
///
/// A value cannot hold a space or an equals sign directly, so a server that needs one sends its hex
/// code. An incomplete or invalid escape is left as written.
fn unescape(value: &str) -> String {
    if !value.contains("\\x") {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find("\\x") {
        let (before, after) = rest.split_at(at);
        out.push_str(before);
        if let Some(byte) = after.get(2..4).and_then(|h| u8::from_str_radix(h, 16).ok()) {
            out.push(byte as char);
            rest = after.get(4..).unwrap_or_default();
        } else {
            out.push_str("\\x");
            rest = after.get(2..).unwrap_or_default();
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isupport(tokens: &[&str]) -> Isupport {
        let mut isupport = Isupport::default();
        for token in tokens {
            let _ = isupport.apply(token);
        }
        isupport
    }

    #[test]
    fn parses_prefix_into_ranked_pairs() {
        let prefix = Prefix::parse("(qaohv)~&@%+").expect("valid prefix");
        assert_eq!(prefix.char_for_mode('o'), Some('@'));
        assert_eq!(prefix.mode_for_char('%'), Some('h'));
        assert_eq!(prefix.rank('~'), Some(0));
        assert!(prefix.rank('@') > prefix.rank('&'));
        assert!(prefix.is_membership_mode('v'));
        assert!(!prefix.is_membership_mode('b'));
    }

    #[test]
    fn rejects_a_prefix_whose_halves_do_not_match() {
        assert!(Prefix::parse("(ov)@").is_none());
        assert!(Prefix::parse("ov)@+").is_none());
        assert!(Prefix::parse("()").is_none());
    }

    #[test]
    fn a_malformed_prefix_keeps_the_previous_value() {
        let isupport = isupport(&["PREFIX=(qaohv)~&@%+", "PREFIX=nonsense"]);
        assert_eq!(isupport.prefix().mode_for_char('~'), Some('q'));
    }

    #[test]
    fn splits_every_prefix_off_a_names_entry() {
        let prefix = Prefix::parse("(qaohv)~&@%+").expect("valid prefix");
        assert_eq!(prefix.split("@+nick"), ("@+", "nick"));
        assert_eq!(prefix.split("nick"), ("", "nick"));
        assert_eq!(prefix.split("~&@%+nick"), ("~&@%+", "nick"));
    }

    #[test]
    fn parses_chanmodes_into_four_classes() {
        let modes = ChanModes::parse("beI,k,l,imnpstn");
        assert_eq!(modes.list, "beI");
        assert_eq!(modes.always_arg, "k");
        assert_eq!(modes.arg_on_set, "l");
        assert_eq!(modes.flag, "imnpstn");
    }

    #[test]
    fn a_short_chanmodes_leaves_later_classes_empty() {
        let modes = ChanModes::parse("b,k");
        assert_eq!(modes.arg_on_set, "");
        assert_eq!(modes.flag, "");
    }

    #[test]
    fn chantypes_defaults_when_the_server_is_silent() {
        let isupport = Isupport::default();
        assert!(isupport.is_channel("#chan"));
        assert!(isupport.is_channel("&chan"));
        assert!(!isupport.is_channel("^voice"));
    }

    #[test]
    fn chantypes_covers_the_obby_voice_and_stream_prefixes() {
        let isupport = isupport(&["CHANTYPES=#^$"]);
        assert!(isupport.is_channel("^general"));
        assert!(isupport.is_channel("$radio"));
        assert!(!isupport.is_channel("&chan"));
        assert!(!isupport.is_channel("nick"));
    }

    #[test]
    fn a_negated_token_restores_the_default() {
        let mut isupport = isupport(&["CASEMAPPING=ascii"]);
        assert_eq!(isupport.casemapping(), Casemapping::Ascii);
        isupport.apply("-CASEMAPPING");
        assert_eq!(isupport.casemapping(), Casemapping::Rfc1459);
        assert!(!isupport.has("CASEMAPPING"));
    }

    #[test]
    fn reads_a_numeric_token() {
        let isupport = isupport(&["LINELEN=1024", "MONITOR=100", "NETWORK=obby"]);
        assert_eq!(isupport.number("LINELEN"), Some(1024));
        assert_eq!(isupport.number("NETWORK"), None);
        assert_eq!(isupport.number("MISSING"), None);
    }

    #[test]
    fn reads_a_per_command_target_limit() {
        let isupport = isupport(&["TARGMAX=PRIVMSG:4,WHOIS:1,JOIN:"]);
        assert_eq!(isupport.targmax("PRIVMSG"), Some(4));
        assert_eq!(isupport.targmax("privmsg"), Some(4));
        assert_eq!(
            isupport.targmax("JOIN"),
            None,
            "an empty limit means unlimited"
        );
        assert_eq!(isupport.targmax("KICK"), None);
    }

    #[test]
    fn unescapes_a_hex_escape_in_a_value() {
        assert_eq!(unescape(r"a\x20b"), "a b");
        assert_eq!(unescape(r"a\x3Db"), "a=b");
        assert_eq!(unescape("plain"), "plain");
    }

    #[test]
    fn leaves_a_broken_escape_as_written() {
        assert_eq!(unescape(r"a\xZZb"), r"a\xZZb");
        assert_eq!(unescape(r"a\x2"), r"a\x2");
    }

    #[test]
    fn appends_a_value_split_across_lines() {
        let isupport = isupport(&["CHANTYPES=#", "CHANTYPES+=^$"]);
        assert_eq!(isupport.get("CHANTYPES"), Some(Some("#^$")));
        assert!(
            isupport.is_channel("^voice"),
            "the typed view sees the cumulative value"
        );
        assert!(isupport.is_channel("#chan"));
    }

    #[test]
    fn appends_onto_nothing_when_the_token_was_absent() {
        let isupport = isupport(&["ELIST+=CTU"]);
        assert_eq!(isupport.get("ELIST"), Some(Some("CTU")));
    }

    #[test]
    fn an_append_unescapes_each_fragment_before_joining() {
        let isupport = isupport(&[r"NETWORK=obby", r"NETWORK+=\x20net"]);
        assert_eq!(isupport.get("NETWORK"), Some(Some("obby net")));
    }

    #[test]
    fn reports_the_name_without_its_grammar_markers() {
        let mut isupport = Isupport::default();
        assert_eq!(
            isupport.apply("CHANTYPES=#"),
            Token {
                name: "CHANTYPES".to_owned(),
                value: Some("#".to_owned()),
                removed: false
            }
        );
        assert_eq!(
            isupport.apply("CHANTYPES+=^"),
            Token {
                name: "CHANTYPES".to_owned(),
                value: Some("#^".to_owned()),
                removed: false
            },
            "an append reports the accumulated value under the bare name"
        );
        assert_eq!(
            isupport.apply("-CHANTYPES"),
            Token {
                name: "CHANTYPES".to_owned(),
                value: None,
                removed: true
            },
            "a removal reports the bare name, never the leading dash"
        );
        assert_eq!(
            isupport.apply("SAFELIST"),
            Token {
                name: "SAFELIST".to_owned(),
                value: None,
                removed: false
            }
        );
    }

    #[test]
    fn recognises_a_status_message_prefix() {
        let isupport = isupport(&["STATUSMSG=@+"]);
        assert!(isupport.is_statusmsg('@'));
        assert!(!isupport.is_statusmsg('#'));
    }
}
