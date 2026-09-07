//! A minimal JSON value, just enough for the shapes the Obby extensions put on the wire.
//!
//! Voice signalling and the channel-bots announcements both carry JSON, and this crate has no JSON
//! library in its dependency graph to lean on: everything here has to build for
//! `wasm32-unknown-unknown` and for `no_std`. The write half lives in `voice.rs`, which is the only
//! thing here that builds JSON; everything else reads it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Num(i64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub(crate) fn parse(text: &str) -> Option<Json> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            pos: 0,
        };
        parser.skip_ws();
        let value = parser.parse_value()?;
        parser.skip_ws();
        if parser.pos == parser.bytes.len() {
            Some(value)
        } else {
            None
        }
    }

    pub(crate) fn field<'a>(&'a self, key: &str) -> Option<&'a Json> {
        match self {
            Json::Object(fields) => fields
                .iter()
                .find(|(k, _)| k.as_str() == key)
                .map(|(_, v)| v),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub(crate) fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }
}

pub(crate) fn field_string(value: &Json, key: &str) -> Option<String> {
    value.field(key)?.as_str().map(str::to_string)
}

/// A cursor over the bytes of a `&str`, parsing exactly the JSON grammar this crate needs.
///
/// Scanning byte-by-byte for the ASCII structural characters (`"`, `\`, braces, brackets, comma,
/// colon, digits, whitespace) is UTF-8-safe: every continuation and multi-byte leading byte in a
/// valid `&str` is outside the ASCII range those bytes occupy, so it can never be mistaken for
/// one, and any run of bytes between two such markers is copied through as a valid UTF-8 slice.
struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek();
        if byte.is_some() {
            self.pos += 1;
        }
        byte
    }

    fn expect(&mut self, byte: u8) -> Option<()> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Some(())
        } else {
            None
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn parse_value(&mut self) -> Option<Json> {
        self.skip_ws();
        match self.peek()? {
            b'"' => self.parse_string().map(Json::Str),
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b't' => self.parse_literal("true", Json::Bool(true)),
            b'f' => self.parse_literal("false", Json::Bool(false)),
            b'n' => self.parse_literal("null", Json::Null),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => None,
        }
    }

    fn parse_literal(&mut self, text: &str, value: Json) -> Option<Json> {
        let end = self.pos + text.len();
        if self.bytes.get(self.pos..end)? == text.as_bytes() {
            self.pos = end;
            Some(value)
        } else {
            None
        }
    }

    fn parse_number(&mut self) -> Option<Json> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        self.skip_digits();
        let int_end = self.pos;
        if self.peek() == Some(b'.') {
            self.pos += 1;
            self.skip_digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            self.skip_digits();
        }
        let text = core::str::from_utf8(self.bytes.get(start..int_end)?).ok()?;
        text.parse::<i64>().ok().map(Json::Num)
    }

    fn skip_digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
    }

    fn parse_array(&mut self) -> Option<Json> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Some(Json::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.bump()? {
                b',' => self.skip_ws(),
                b']' => return Some(Json::Array(items)),
                _ => return None,
            }
        }
    }

    fn parse_object(&mut self) -> Option<Json> {
        self.expect(b'{')?;
        let mut fields = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Some(Json::Object(fields));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            let value = self.parse_value()?;
            fields.push((key, value));
            self.skip_ws();
            match self.bump()? {
                b',' => {}
                b'}' => return Some(Json::Object(fields)),
                _ => return None,
            }
        }
    }

    fn parse_string(&mut self) -> Option<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        let mut start = self.pos;
        loop {
            let byte = self.peek()?;
            match byte {
                b'"' => {
                    out.push_str(core::str::from_utf8(self.bytes.get(start..self.pos)?).ok()?);
                    self.pos += 1;
                    return Some(out);
                }
                b'\\' => {
                    out.push_str(core::str::from_utf8(self.bytes.get(start..self.pos)?).ok()?);
                    self.pos += 1;
                    self.parse_escape(&mut out)?;
                    start = self.pos;
                }
                _ => self.pos += 1,
            }
        }
    }

    fn parse_escape(&mut self, out: &mut String) -> Option<()> {
        match self.bump()? {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{8}'),
            b'f' => out.push('\u{c}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => out.push(self.parse_unicode_escape()?),
            _ => return None,
        }
        Some(())
    }

    fn parse_unicode_escape(&mut self) -> Option<char> {
        let unit = self.parse_hex4()?;
        if (0xD800..=0xDBFF).contains(&unit) {
            self.expect(b'\\')?;
            self.expect(b'u')?;
            let low = self.parse_hex4()?;
            if !(0xDC00..=0xDFFF).contains(&low) {
                return None;
            }
            let scalar = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
            char::from_u32(scalar)
        } else {
            char::from_u32(unit)
        }
    }

    fn parse_hex4(&mut self) -> Option<u32> {
        let slice = self.bytes.get(self.pos..self.pos + 4)?;
        let text = core::str::from_utf8(slice).ok()?;
        let value = u32::from_str_radix(text, 16).ok()?;
        self.pos += 4;
        Some(value)
    }
}
