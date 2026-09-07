//! One IRC protocol line.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::tags::Tags;

/// Why a line could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The line was empty, or held only tags and a source with no command after them.
    #[error("the line carries no command")]
    MissingCommand,
}

/// Where a message came from, as sent in the `:`-prefixed source.
///
/// A server sends its own name; a client's message arrives as `nick!user@host`, though a server may
/// send only the nick.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "MessageSource"))]
pub struct Source {
    /// The nick, or the server name when there is no `!` or `@`.
    pub name: String,
    /// The user part, when the source is a full hostmask.
    pub user: Option<String>,
    /// The host part, when present.
    pub host: Option<String>,
}

impl Source {
    /// Split a raw source into its parts. Never fails: a source with no `!` or `@` is all `name`.
    pub fn parse(raw: &str) -> Self {
        let (name, rest) = raw
            .split_once('!')
            .map_or((raw, None), |(n, r)| (n, Some(r)));
        if let Some(rest) = rest {
            let (user, host) = rest
                .split_once('@')
                .map_or((rest, None), |(u, h)| (u, Some(h)));
            Self {
                name: name.to_owned(),
                user: Some(user.to_owned()),
                host: host.map(ToOwned::to_owned),
            }
        } else {
            let (name, host) = name
                .split_once('@')
                .map_or((name, None), |(n, h)| (n, Some(h)));
            Self {
                name: name.to_owned(),
                user: None,
                host: host.map(ToOwned::to_owned),
            }
        }
    }

    /// True when the source names a server rather than a user, which we infer from the absence of a
    /// user part and the presence of a dot.
    ///
    /// There is no reliable signal on the wire, so a nick containing a dot is indistinguishable from
    /// a server name. Prefer comparing against the server name from `001` when you have it.
    pub fn looks_like_server(&self) -> bool {
        self.user.is_none() && self.name.contains('.')
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        if let Some(user) = &self.user {
            write!(f, "!{user}")?;
        }
        if let Some(host) = &self.host {
            write!(f, "@{host}")?;
        }
        Ok(())
    }
}

/// A parsed protocol line.
///
/// The types are owned rather than borrowed from the input. A borrowed `Message<'a>` would parse
/// faster, but every consumer of this crate reaches it across a language boundary that cannot carry
/// a Rust lifetime, so the copy has to happen somewhere and here is the only place it happens once.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "RawMessage"))]
pub struct Message {
    /// The tag section, empty when the line carried no `@`.
    pub tags: Tags,
    /// The source, when the line carried one.
    pub source: Option<Source>,
    /// The command or three-digit numeric, as it arrived. Commands are case-insensitive on the wire,
    /// so compare with [`Message::is`] rather than `==`.
    pub command: String,
    /// The parameters, with the trailing parameter last and its `:` removed.
    pub params: Vec<String>,
}

impl Message {
    /// Build a message with no tags and no source.
    pub fn new(command: impl Into<String>, params: impl IntoIterator<Item: Into<String>>) -> Self {
        Self {
            tags: Tags::default(),
            source: None,
            command: command.into(),
            params: params.into_iter().map(Into::into).collect(),
        }
    }

    /// Parse one line. A trailing CRLF is optional and is ignored.
    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let mut rest = line.trim_end_matches(['\r', '\n']);

        let mut tags = Tags::default();
        if let Some(after) = rest.strip_prefix('@') {
            let (raw, remainder) = after.split_once(' ').ok_or(ParseError::MissingCommand)?;
            tags = Tags::parse(raw);
            rest = remainder.trim_start_matches(' ');
        }

        let mut source = None;
        if let Some(after) = rest.strip_prefix(':') {
            let (raw, remainder) = after.split_once(' ').ok_or(ParseError::MissingCommand)?;
            source = Some(Source::parse(raw));
            rest = remainder.trim_start_matches(' ');
        }

        let (command, mut rest) = rest.split_once(' ').unwrap_or((rest, ""));
        if command.is_empty() {
            return Err(ParseError::MissingCommand);
        }

        let mut params = Vec::new();
        loop {
            rest = rest.trim_start_matches(' ');
            if rest.is_empty() {
                break;
            }
            if let Some(trailing) = rest.strip_prefix(':') {
                params.push(trailing.to_owned());
                break;
            }
            if let Some((param, remainder)) = rest.split_once(' ') {
                params.push(param.to_owned());
                rest = remainder;
            } else {
                params.push(rest.to_owned());
                break;
            }
        }

        Ok(Self {
            tags,
            source,
            command: command.to_owned(),
            params,
        })
    }

    /// Compare the command case-insensitively, which is how the wire defines it.
    pub fn is(&self, command: &str) -> bool {
        self.command.eq_ignore_ascii_case(command)
    }

    /// The parameter at this position, if the message has one.
    pub fn param(&self, index: usize) -> Option<&str> {
        self.params.get(index).map(String::as_str)
    }

    /// The last parameter, which for most commands is the message body.
    pub fn trailing(&self) -> Option<&str> {
        self.params.last().map(String::as_str)
    }

    /// The unescaped value of a tag.
    pub fn tag(&self, key: &str) -> Option<&str> {
        self.tags.get(key)
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.tags.is_empty() {
            write!(f, "@{} ", self.tags)?;
        }
        if let Some(source) = &self.source {
            write!(f, ":{source} ")?;
        }
        f.write_str(&self.command)?;
        let last = self.params.len().saturating_sub(1);
        for (i, param) in self.params.iter().enumerate() {
            // a parameter that is empty, holds a space, or starts with a colon can only travel as
            // the trailing one, so it has to keep its marker
            if i == last && (param.is_empty() || param.contains(' ') || param.starts_with(':')) {
                write!(f, " :{param}")?;
            } else {
                write!(f, " {param}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;

    fn parse(line: &str) -> Message {
        Message::parse(line).expect("line should parse")
    }

    #[test]
    fn parses_a_bare_command() {
        let msg = parse("PING");
        assert!(msg.is("ping"));
        assert!(msg.params.is_empty());
        assert!(msg.source.is_none());
    }

    #[test]
    fn parses_source_command_and_trailing() {
        let msg = parse(":nick!user@host PRIVMSG #chan :hello world");
        let source = msg.source.as_ref().expect("source");
        assert_eq!(source.name, "nick");
        assert_eq!(source.user.as_deref(), Some("user"));
        assert_eq!(source.host.as_deref(), Some("host"));
        assert_eq!(msg.params, vec!["#chan", "hello world"]);
    }

    #[test]
    fn parses_a_source_that_is_only_a_nick() {
        let msg = parse(":nick JOIN #chan");
        let source = msg.source.as_ref().expect("source");
        assert_eq!(source.name, "nick");
        assert!(source.user.is_none());
        assert!(source.host.is_none());
    }

    #[test]
    fn parses_a_nick_with_a_host_but_no_user() {
        let source = Source::parse("nick@host");
        assert_eq!(source.name, "nick");
        assert!(source.user.is_none());
        assert_eq!(source.host.as_deref(), Some("host"));
    }

    #[test]
    fn keeps_an_empty_trailing_parameter() {
        let msg = parse("PRIVMSG #chan :");
        assert_eq!(msg.params, vec!["#chan", ""]);
    }

    #[test]
    fn keeps_a_colon_inside_the_trailing_parameter() {
        let msg = parse("PRIVMSG #chan :a : b");
        assert_eq!(msg.trailing(), Some("a : b"));
    }

    #[test]
    fn tolerates_repeated_spaces_between_parameters() {
        let msg = parse(":s 353 me =  #chan  :a b");
        assert_eq!(msg.params, vec!["me", "=", "#chan", "a b"]);
    }

    #[test]
    fn parses_tags_with_a_source() {
        let msg = parse("@time=2026-09-06T10:00:00.000Z;+draft/reply=abc :n!u@h TAGMSG #c");
        assert_eq!(msg.tag("time"), Some("2026-09-06T10:00:00.000Z"));
        assert_eq!(msg.tag("+draft/reply"), Some("abc"));
        assert!(msg.is("TAGMSG"));
    }

    #[test]
    fn rejects_tags_with_no_command() {
        assert_eq!(Message::parse("@a=b"), Err(ParseError::MissingCommand));
        assert_eq!(Message::parse(":source"), Err(ParseError::MissingCommand));
        assert_eq!(Message::parse(""), Err(ParseError::MissingCommand));
    }

    #[test]
    fn round_trips_a_full_line() {
        let line = "@id=1;+obby.world/e2ee=blob :n!u@h PRIVMSG #chan :hello world";
        assert_eq!(format!("{}", parse(line)), line);
    }

    #[test]
    fn writes_a_trailing_marker_only_where_it_is_needed() {
        assert_eq!(format!("{}", Message::new("JOIN", ["#chan"])), "JOIN #chan");
        assert_eq!(
            format!("{}", Message::new("PRIVMSG", ["#c", "a b"])),
            "PRIVMSG #c :a b"
        );
        assert_eq!(
            format!("{}", Message::new("PRIVMSG", ["#c", ""])),
            "PRIVMSG #c :"
        );
        assert_eq!(
            format!("{}", Message::new("PRIVMSG", ["#c", ":o"])),
            "PRIVMSG #c ::o"
        );
    }

    #[test]
    fn ignores_the_trailing_line_ending() {
        assert_eq!(parse("PING :x\r\n"), parse("PING :x"));
    }

    proptest::proptest! {
        #[test]
        fn any_message_survives_a_round_trip(
            command in "[A-Z]{1,8}",
            params in proptest::collection::vec("[^ \r\n:][^ \r\n]{0,16}", 0..4),
            trailing in "[^\r\n]{0,32}",
        ) {
            let mut msg = Message::new(command, params);
            msg.params.push(trailing);
            let rendered = format!("{msg}");
            proptest::prop_assert_eq!(Message::parse(&rendered).expect("round trip"), msg);
        }
    }
}
