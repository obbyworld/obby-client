//! Message tags and their escaping.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::fmt::Write as _;

/// One message tag. A tag with an empty value is the same as a tag with no value, so both parse to
/// `value: None`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Tag {
    /// The tag name, including a leading `+` on a client-only tag and any vendor prefix.
    pub key: String,
    /// The unescaped value.
    pub value: Option<String>,
}

impl Tag {
    /// A tag that carries a value.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: Some(value.into()),
        }
    }

    /// A tag that is only present, with no value.
    pub fn flag(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: None,
        }
    }

    /// True when this is a client-only tag, which servers relay without interpreting.
    pub fn is_client_only(&self) -> bool {
        self.key.starts_with('+')
    }
}

/// The tag section of a message, in the order it arrived.
///
/// Order is kept rather than folded into a map because a round trip has to reproduce the line, and
/// because a server may legally send the same key twice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Tags(Vec<Tag>);

impl Tags {
    /// Parse the raw tag section, without its leading `@`.
    pub fn parse(raw: &str) -> Self {
        Self(
            raw.split(';')
                .filter(|part| !part.is_empty())
                .map(|part| match part.split_once('=') {
                    None | Some((_, "")) => Tag::flag(key_of(part)),
                    Some((key, value)) => Tag::new(key, unescape(value)),
                })
                .collect(),
        )
    }

    /// The unescaped value of the last tag with this key, if it has one.
    ///
    /// A key may legally appear twice, and the specification says to disregard all but the final
    /// occurrence. The duplicates are still kept so a line renders back exactly as it arrived.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|tag| tag.key == key)?
            .value
            .as_deref()
    }

    /// True when a tag with this key is present, with or without a value.
    pub fn contains(&self, key: &str) -> bool {
        self.0.iter().any(|tag| tag.key == key)
    }

    /// Add a tag, replacing any existing tag with the same key.
    pub fn set(&mut self, tag: Tag) {
        match self.0.iter_mut().find(|existing| existing.key == tag.key) {
            Some(existing) => *existing = tag,
            None => self.0.push(tag),
        }
    }

    /// Remove every tag with this key.
    pub fn remove(&mut self, key: &str) {
        self.0.retain(|tag| tag.key != key);
    }

    /// True when there are no tags, in which case the line carries no `@` section.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many tags are present.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Iterate the tags in wire order.
    pub fn iter(&self) -> core::slice::Iter<'_, Tag> {
        self.0.iter()
    }
}

impl FromIterator<Tag> for Tags {
    fn from_iter<T: IntoIterator<Item = Tag>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<'a> IntoIterator for &'a Tags {
    type Item = &'a Tag;
    type IntoIter = core::slice::Iter<'a, Tag>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl fmt::Display for Tags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, tag) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(";")?;
            }
            f.write_str(&tag.key)?;
            if let Some(value) = &tag.value {
                write!(f, "={}", Escaped(value))?;
            }
        }
        Ok(())
    }
}

fn key_of(part: &str) -> &str {
    part.split_once('=').map_or(part, |(key, _)| key)
}

/// Undo the escaping a tag value carries on the wire.
///
/// A backslash before any character with no escape of its own drops the backslash and keeps the
/// character, and a lone trailing backslash is dropped, both per the message-tags specification.
pub(crate) fn unescape(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(':') => out.push(';'),
            Some('s') => out.push(' '),
            Some('\\') => out.push('\\'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Wraps a tag value so that `Display` writes it escaped.
pub(crate) struct Escaped<'a>(pub &'a str);

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            match c {
                ';' => f.write_str("\\:")?,
                ' ' => f.write_str("\\s")?,
                '\\' => f.write_str("\\\\")?,
                '\r' => f.write_str("\\r")?,
                '\n' => f.write_str("\\n")?,
                other => f.write_char(other)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn unescapes_the_specified_sequences() {
        assert_eq!(unescape(r"a\:b"), "a;b");
        assert_eq!(unescape(r"a\sb"), "a b");
        assert_eq!(unescape(r"a\\b"), r"a\b");
        assert_eq!(unescape(r"a\rb"), "a\rb");
        assert_eq!(unescape(r"a\nb"), "a\nb");
    }

    #[test]
    fn drops_the_backslash_of_an_undefined_escape() {
        assert_eq!(unescape(r"a\qb"), "aqb");
    }

    #[test]
    fn drops_a_lone_trailing_backslash() {
        assert_eq!(unescape(r"ab\"), "ab");
    }

    #[test]
    fn escapes_round_trip() {
        let raw = "semi;space colon:back\\slash\r\n";
        let escaped = format!("{}", Escaped(raw));
        assert_eq!(unescape(&escaped), raw);
    }

    #[test]
    fn an_empty_value_is_the_same_as_no_value() {
        let tags = Tags::parse("a=;b");
        assert_eq!(tags.get("a"), None);
        assert!(tags.contains("a"));
        assert!(tags.contains("b"));
    }

    #[test]
    fn keeps_wire_order_and_duplicate_keys() {
        let tags = Tags::parse("z=1;a=2;z=3");
        assert_eq!(tags.len(), 3);
        assert_eq!(
            tags.get("z"),
            Some("3"),
            "the specification says to disregard all but the final occurrence"
        );
        assert_eq!(
            format!("{tags}"),
            "z=1;a=2;z=3",
            "but the line still renders as it arrived"
        );
    }

    #[test]
    fn recognises_a_client_only_tag() {
        assert!(Tag::flag("+obby.world/e2ee").is_client_only());
        assert!(!Tag::flag("time").is_client_only());
    }
}
