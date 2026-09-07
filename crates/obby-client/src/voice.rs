//! Voice signalling: the `+obsidianirc/rtc` frame types and room state.
//!
//! This is the signalling and room-state plane only: parsing and building `+obsidianirc/rtc`
//! frames, splitting and
//! reassembling an oversized `offer`/`answer`, and tracking who is in a room and what they are
//! doing. `RTCPeerConnection`, codecs and `getUserMedia` never appear here; an `sdp` value is an
//! opaque string this module chunks and reassembles, never parses.
//!
//! The published <https://github.com/obbyworld/extensions/blob/main/voice.md> documents 9 of the
//! 19 frame types actually on the
//! wire, so [`Signal`] follows the wire. [`Signal::to_json`] and [`Signal::from_json`] implement that
//! wire's JSON directly, by hand: this crate has no JSON library in its dependency graph to lean
//! on, and the exact shape (three incompatible `presence` bodies under one `type`, chunk fields
//! flattened onto `offer`/`answer` rather than nested, a redundant `state` on `speaking`/
//! `silent`) is precise enough that a generic derive could not produce it without the same
//! amount of per-field annotation this hand-written codec already is. The
//! `#[cfg_attr(feature = "serde", derive(...))]` on every public type here is the same generic,
//! non-wire convenience every other module in this crate offers; it does not attempt to match
//! the wire shape.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;

use obby_proto::{CaseFolded, Casemapping};

// ---------------------------------------------------------------------------------------------
// A minimal JSON value, just enough to encode and decode the shapes this module needs.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(i64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    fn parse(text: &str) -> Option<Json> {
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

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => {
                let _ = write!(out, "{n}");
            }
            Json::Str(s) => write_json_string(s, out),
            Json::Array(items) => write_json_array(items, out),
            Json::Object(fields) => write_json_object(fields, out),
        }
    }

    fn field<'a>(&'a self, key: &str) -> Option<&'a Json> {
        match self {
            Json::Object(fields) => fields
                .iter()
                .find(|(k, _)| k.as_str() == key)
                .map(|(_, v)| v),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn as_u32(&self) -> Option<u32> {
        match self {
            Json::Num(n) => u32::try_from(*n).ok(),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }
}

fn write_json_array(items: &[Json], out: &mut String) {
    out.push('[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        item.write(out);
    }
    out.push(']');
}

fn write_json_object(fields: &[(String, Json)], out: &mut String) {
    out.push('{');
    for (i, (key, value)) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_json_string(key, out);
        out.push(':');
        value.write(out);
    }
    out.push('}');
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (u32::from(c)) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A cursor over the bytes of a `&str`, parsing exactly the JSON grammar this module needs.
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

fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Object(
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

fn opt_field(fields: &mut Vec<(String, Json)>, key: &str, value: Option<Json>) {
    if let Some(value) = value {
        fields.push((key.to_string(), value));
    }
}

fn strings_json(items: &[String]) -> Json {
    Json::Array(items.iter().map(|s| Json::Str(s.clone())).collect())
}

fn strings(value: &Json) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_string))
        .collect()
}

fn field_string(value: &Json, key: &str) -> Option<String> {
    value.field(key)?.as_str().map(str::to_string)
}

fn field_string_opt(value: &Json, key: &str) -> Option<String> {
    value.field(key).and_then(Json::as_str).map(str::to_string)
}

fn field_u32(value: &Json, key: &str) -> Option<u32> {
    value.field(key)?.as_u32()
}

// ---------------------------------------------------------------------------------------------
// Small enumerated wire values shared by several frame types.
// ---------------------------------------------------------------------------------------------

/// The two states an intent frame like `mic` or `hand` toggles between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub enum OnOff {
    /// The feature is enabled.
    On,
    /// The feature is disabled.
    Off,
}

impl OnOff {
    fn wire(self) -> &'static str {
        match self {
            OnOff::On => "on",
            OnOff::Off => "off",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "on" => Some(OnOff::On),
            "off" => Some(OnOff::Off),
            _ => None,
        }
    }
}

/// Whether a room participant may publish audio and video, or only receive it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "VoiceRole"))]
pub enum Role {
    /// May publish: everyone in a `^` room, and the streamer plus their promotions in a `$` room.
    Publisher,
    /// Receives only: never true in a `^` room; anyone not promoted in a `$` room.
    Viewer,
}

impl Role {
    fn wire(self) -> &'static str {
        match self {
            Role::Publisher => "streamer",
            Role::Viewer => "viewer",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "streamer" => Some(Role::Publisher),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }
}

/// Who may publish in a voice room, decided by the channel's sigil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "VoiceRoomKind"))]
pub enum RoomKind {
    /// A `^` channel: every member publishes their own microphone for free.
    Publish,
    /// A `$` channel: a streamer and whoever they promote publish; everyone else watches.
    Stream,
}

impl RoomKind {
    fn wire(self) -> &'static str {
        match self {
            RoomKind::Publish => "voice",
            RoomKind::Stream => "stream",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "voice" => Some(RoomKind::Publish),
            "stream" => Some(RoomKind::Stream),
            _ => None,
        }
    }

    /// The kind a channel's own name implies: `$` streams, everything else publishes.
    fn for_channel(channel: &str) -> Self {
        if channel.starts_with('$') {
            RoomKind::Stream
        } else {
            RoomKind::Publish
        }
    }
}

/// Which per-participant toggle a `presence` notification reports, for its toggle sub-shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "VoiceToggle"))]
pub enum ToggleKind {
    /// Microphone.
    Mic,
    /// Camera.
    Video,
    /// Screen share.
    Screen,
    /// Raised hand.
    Hand,
}

impl ToggleKind {
    fn wire(self) -> &'static str {
        match self {
            ToggleKind::Mic => "mic",
            ToggleKind::Video => "video",
            ToggleKind::Screen => "screen",
            ToggleKind::Hand => "hand",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "mic" => Some(ToggleKind::Mic),
            "video" => Some(ToggleKind::Video),
            "screen" => Some(ToggleKind::Screen),
            "hand" => Some(ToggleKind::Hand),
            _ => None,
        }
    }

    fn apply(self, participant: &mut Participant, state: OnOff) {
        match self {
            ToggleKind::Mic => participant.mic = state,
            ToggleKind::Video => participant.video = state,
            ToggleKind::Screen => participant.screen = state,
            ToggleKind::Hand => participant.hand = state,
        }
    }
}

/// The `state` a `presence` notification carries.
///
/// One wire `type: "presence"` actually carries three incompatible shapes: a membership change (`Joined`/`Left`), a toggle
/// (`On`/`Off`, read together with [`Signal::Presence`]'s `kind`), or an activity flag
/// (`Speaking`/`Silent`/`DeafOn`/`DeafOff`, which carries no `kind` at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "VoicePresence"))]
pub enum PresenceState {
    /// `member` joined the room. Carries a `role` only in a `$` room.
    Joined,
    /// `member` left the room.
    Left,
    /// The toggle named by `kind` turned on.
    On,
    /// The toggle named by `kind` turned off.
    Off,
    /// `member` started talking, as detected by their own voice-activity detector.
    Speaking,
    /// `member` stopped talking.
    Silent,
    /// `member` deafened themself.
    DeafOn,
    /// `member` un-deafened themself.
    DeafOff,
}

impl PresenceState {
    fn wire(self) -> &'static str {
        match self {
            PresenceState::Joined => "joined",
            PresenceState::Left => "left",
            PresenceState::On => "on",
            PresenceState::Off => "off",
            PresenceState::Speaking => "speaking",
            PresenceState::Silent => "silent",
            PresenceState::DeafOn => "deaf-on",
            PresenceState::DeafOff => "deaf-off",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "joined" => PresenceState::Joined,
            "left" => PresenceState::Left,
            "on" => PresenceState::On,
            "off" => PresenceState::Off,
            "speaking" => PresenceState::Speaking,
            "silent" => PresenceState::Silent,
            "deaf-on" => PresenceState::DeafOn,
            "deaf-off" => PresenceState::DeafOff,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Supporting frame payloads.
// ---------------------------------------------------------------------------------------------

/// A hint from the SFU mapping one negotiated media line to the member it belongs to.
///
/// The SFU sends mid-to-member hints so an inbound track can be attributed to the right member
/// when the SDP's own `msid` is missing or unreliable. This is the minimal shape that serves that
/// purpose; a real server may send more fields, which this simply ignores on decode.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "VoiceTrackHint"))]
pub struct TrackHint {
    /// The SDP media line identifier this hint names.
    pub mid: String,
    /// The member that media line belongs to.
    pub member: String,
}

impl TrackHint {
    fn to_json(&self) -> Json {
        obj(alloc::vec![
            ("mid", Json::Str(self.mid.clone())),
            ("member", Json::Str(self.member.clone())),
        ])
    }

    fn from_json(value: &Json) -> Option<Self> {
        Some(Self {
            mid: field_string(value, "mid")?,
            member: field_string(value, "member")?,
        })
    }
}

/// TURN/STUN credentials the SFU hands us on `joined`.
///
/// These are short-lived, and nothing in the `joined` handshake or anywhere else in the
/// signalling plane ever refreshes them mid-call. A call that outlives them loses its relay path
/// with no warning;
/// whoever integrates this signalling plane needs to leave and rejoin (or otherwise trigger a
/// fresh `joined`) before that happens, since nothing here does it automatically.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct TurnCredentials {
    /// The TURN/STUN server URLs to try, in order.
    pub urls: Vec<String>,
    /// The short-lived TURN username.
    pub username: String,
    /// The short-lived TURN password.
    pub password: String,
}

impl TurnCredentials {
    fn to_json(&self) -> Json {
        obj(alloc::vec![
            ("urls", strings_json(&self.urls)),
            ("username", Json::Str(self.username.clone())),
            ("password", Json::Str(self.password.clone())),
        ])
    }

    fn from_json(value: &Json) -> Option<Self> {
        let urls = match value.field("urls")? {
            Json::Str(one) => alloc::vec![one.clone()],
            many @ Json::Array(_) => strings(many)?,
            _ => return None,
        };
        Some(Self {
            urls,
            username: field_string(value, "username")?,
            password: field_string(value, "password")?,
        })
    }
}

/// The chunk-correlation fields riding alongside a split `sdp` value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct ChunkMeta {
    /// The id every chunk of one split frame shares.
    pub id: String,
    /// This chunk's 0-based position.
    pub seq: u32,
    /// How many chunks the split frame was cut into.
    pub total: u32,
}

fn decode_chunk(value: &Json) -> Option<ChunkMeta> {
    Some(ChunkMeta {
        id: field_string_opt(value, "id")?,
        seq: field_u32(value, "seq")?,
        total: field_u32(value, "total")?,
    })
}

fn chunk_meta_fields(chunk: &ChunkMeta) -> Vec<(String, Json)> {
    alloc::vec![
        ("id".to_string(), Json::Str(chunk.id.clone())),
        ("seq".to_string(), Json::Num(i64::from(chunk.seq))),
        ("total".to_string(), Json::Num(i64::from(chunk.total))),
    ]
}

// ---------------------------------------------------------------------------------------------
// Signal: every `+obsidianirc/rtc` frame type.
// ---------------------------------------------------------------------------------------------

/// One `+obsidianirc/rtc` signalling frame.
///
/// Every variant is one JSON object's `type`. The published
/// <https://github.com/obbyworld/extensions/blob/main/voice.md> documents only 9 of these, the
/// wire carries 19, and this enum follows the wire. Outbound intent and inbound notification are modelled as distinct shapes where the
/// wire actually distinguishes them (`mic`/`video`/`screen`/`hand`/`speaking`/`silent`/`deaf`
/// versus the `presence` they get rebroadcast as; `promote`/`demote` versus `role`), rather than
/// collapsed into one type as the published table's prose implies.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "VoiceSignal"))]
#[cfg_attr(feature = "serde", serde(tag = "type"))]
#[non_exhaustive]
pub enum Signal {
    /// Ask to join a voice room. Client to server.
    Join {
        /// The channel to join.
        channel: String,
    },
    /// Leave a voice room. Client to server.
    ///
    /// A reconnect should send this before rejoining: a dropped link otherwise leaves the SFU
    /// holding a dead peer for this nick, which answers a plain `join` with "already joined"
    /// rather than a fresh handshake.
    Leave {
        /// The channel to leave.
        channel: String,
    },
    /// The server admitted us to the room. Server to client.
    Joined {
        /// Every current member's nick.
        members: Vec<String>,
        /// The room's kind, present only for a `$` channel.
        mode: Option<RoomKind>,
        /// The role granted to us specifically, present only for a `$` channel.
        role: Option<Role>,
        /// Who currently publishes, for a `$` channel.
        streamers: Option<Vec<String>>,
        /// The TURN credentials for this call.
        turn: Option<TurnCredentials>,
        /// Hints mapping media lines to the members they belong to.
        tracks: Option<Vec<TrackHint>>,
    },
    /// An SDP offer, from either side: ours to the SFU, or the SFU renegotiating with us.
    Offer {
        /// The offer SDP, or one slice of it when `chunk` is set.
        sdp: String,
        /// Track-attribution hints, carried only on chunk 0 or an unchunked offer.
        tracks: Option<Vec<TrackHint>>,
        /// Set when this frame is one of several chunks sharing an id.
        chunk: Option<ChunkMeta>,
    },
    /// An SDP answer, from either side.
    Answer {
        /// The answer SDP, or one slice of it when `chunk` is set.
        sdp: String,
        /// Set when this frame is one of several chunks sharing an id.
        chunk: Option<ChunkMeta>,
    },
    /// One ICE candidate, from either side.
    Ice {
        /// The candidate string.
        cand: String,
        /// The media line it applies to.
        mid: Option<String>,
        /// The media line's index, when `mid` is absent.
        mlineidx: Option<u32>,
    },
    /// A room or participant state change. Server to client only.
    Presence {
        /// Who this is about.
        member: String,
        /// What changed.
        state: PresenceState,
        /// Which toggle, when `state` is [`PresenceState::On`] or [`PresenceState::Off`].
        kind: Option<ToggleKind>,
        /// The role granted, when `state` is [`PresenceState::Joined`] in a `$` room.
        role: Option<Role>,
    },
    /// Our microphone toggled. Client to server; the server rebroadcasts it as `presence`.
    Mic {
        /// The new state.
        state: OnOff,
    },
    /// Our camera toggled.
    Video {
        /// The new state.
        state: OnOff,
    },
    /// Our screen share toggled.
    Screen {
        /// The new state.
        state: OnOff,
    },
    /// Our raised hand toggled.
    Hand {
        /// The new state.
        state: OnOff,
    },
    /// Our deafen toggled.
    ///
    /// A room learns that someone deafened only from this frame, so we send it the way we send
    /// `mic`, `video`, `screen` and `hand`, and the room applies the matching `presence`
    /// `deaf-on`/`deaf-off` like every other toggle.
    Deaf {
        /// The new state.
        state: OnOff,
    },
    /// Our own voice-activity detector says we started talking.
    Speaking,
    /// Our own voice-activity detector says we stopped talking.
    Silent,
    /// React with an emoji. Outbound carries no `member` (the sender is implicit); the server's
    /// rebroadcast adds it.
    React {
        /// Who reacted, present only on the inbound broadcast.
        member: Option<String>,
        /// The emoji.
        emoji: String,
    },
    /// Ask to promote `target` to publisher, in a `$` room. Client to server.
    ///
    /// Only the room's first streamer may do this, and self-demotion is always allowed, but that
    /// rule is enforced only by the server: nothing stops a client from sending this frame
    /// regardless.
    Promote {
        /// The member to promote.
        target: String,
    },
    /// Ask to demote `target` back to viewer, in a `$` room. Client to server.
    Demote {
        /// The member to demote.
        target: String,
    },
    /// The server's authoritative answer to a `promote` or `demote`. Server to client.
    Role {
        /// Whose role changed.
        member: String,
        /// Their new role.
        role: Role,
    },
    /// The server rejected the last request.
    Error {
        /// A human-readable reason, when the server gave one.
        error: Option<String>,
    },
}

impl Signal {
    /// Encode this frame as the JSON object that travels on the wire.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.to_json_value().write(&mut out);
        out
    }

    /// Decode one wire JSON object into a frame.
    ///
    /// Returns `None` for text that is not valid JSON, an object missing a field its `type`
    /// requires, or a `type` this module does not know.
    pub fn from_json(text: &str) -> Option<Self> {
        Self::from_value(&Json::parse(text)?)
    }

    fn to_json_value(&self) -> Json {
        let (kind, mut fields) = self.wire_fields();
        fields.insert(0, ("type".to_string(), Json::Str(kind.to_string())));
        Json::Object(fields)
    }

    fn wire_fields(&self) -> (&'static str, Vec<(String, Json)>) {
        match self {
            Signal::Join { channel } => ("join", channel_fields(channel)),
            Signal::Leave { channel } => ("leave", channel_fields(channel)),
            Signal::Joined {
                members,
                mode,
                role,
                streamers,
                turn,
                tracks,
            } => (
                "joined",
                joined_fields(
                    members,
                    *mode,
                    *role,
                    streamers.as_deref(),
                    turn.as_ref(),
                    tracks.as_deref(),
                ),
            ),
            Signal::Offer { sdp, tracks, chunk } => (
                "offer",
                offer_fields(sdp, tracks.as_deref(), chunk.as_ref()),
            ),
            Signal::Answer { sdp, chunk } => ("answer", answer_fields(sdp, chunk.as_ref())),
            Signal::Ice {
                cand,
                mid,
                mlineidx,
            } => ("ice", ice_fields(cand, mid.as_deref(), *mlineidx)),
            Signal::Presence {
                member,
                state,
                kind,
                role,
            } => ("presence", presence_fields(member, *state, *kind, *role)),
            Signal::Mic { state } => ("mic", state_field(*state)),
            Signal::Video { state } => ("video", state_field(*state)),
            Signal::Screen { state } => ("screen", state_field(*state)),
            Signal::Hand { state } => ("hand", state_field(*state)),
            Signal::Deaf { state } => ("deaf", state_field(*state)),
            Signal::Speaking => ("speaking", literal_state_field("speaking")),
            Signal::Silent => ("silent", literal_state_field("silent")),
            Signal::React { member, emoji } => ("react", react_fields(member.as_deref(), emoji)),
            Signal::Promote { target } => ("promote", target_fields(target)),
            Signal::Demote { target } => ("demote", target_fields(target)),
            Signal::Role { member, role } => ("role", role_fields(member, *role)),
            Signal::Error { error } => ("error", error_fields(error.as_deref())),
        }
    }

    fn from_value(value: &Json) -> Option<Self> {
        let kind = value.field("type")?.as_str()?;
        match kind {
            "join" => Some(Signal::Join {
                channel: field_string(value, "channel")?,
            }),
            "leave" => Some(Signal::Leave {
                channel: field_string(value, "channel")?,
            }),
            "joined" => decode_joined(value),
            "offer" => decode_offer(value),
            "answer" => decode_answer(value),
            "ice" => decode_ice(value),
            "presence" => decode_presence(value),
            "mic" => Some(Signal::Mic {
                state: decode_on_off(value)?,
            }),
            "video" => Some(Signal::Video {
                state: decode_on_off(value)?,
            }),
            "screen" => Some(Signal::Screen {
                state: decode_on_off(value)?,
            }),
            "hand" => Some(Signal::Hand {
                state: decode_on_off(value)?,
            }),
            "deaf" => Some(Signal::Deaf {
                state: decode_on_off(value)?,
            }),
            "speaking" => Some(Signal::Speaking),
            "silent" => Some(Signal::Silent),
            "react" => decode_react(value),
            "promote" => Some(Signal::Promote {
                target: field_string(value, "target")?,
            }),
            "demote" => Some(Signal::Demote {
                target: field_string(value, "target")?,
            }),
            "role" => decode_role(value),
            "error" => Some(Signal::Error {
                error: field_string_opt(value, "error"),
            }),
            _ => None,
        }
    }
}

fn channel_fields(channel: &str) -> Vec<(String, Json)> {
    alloc::vec![("channel".to_string(), Json::Str(channel.to_string()))]
}

fn target_fields(target: &str) -> Vec<(String, Json)> {
    alloc::vec![("target".to_string(), Json::Str(target.to_string()))]
}

fn state_field(state: OnOff) -> Vec<(String, Json)> {
    alloc::vec![("state".to_string(), Json::Str(state.wire().to_string()))]
}

fn literal_state_field(state: &str) -> Vec<(String, Json)> {
    alloc::vec![("state".to_string(), Json::Str(state.to_string()))]
}

fn role_fields(member: &str, role: Role) -> Vec<(String, Json)> {
    alloc::vec![
        ("member".to_string(), Json::Str(member.to_string())),
        ("role".to_string(), Json::Str(role.wire().to_string())),
    ]
}

fn react_fields(member: Option<&str>, emoji: &str) -> Vec<(String, Json)> {
    let mut fields = Vec::new();
    if let Some(member) = member {
        fields.push(("member".to_string(), Json::Str(member.to_string())));
    }
    fields.push(("emoji".to_string(), Json::Str(emoji.to_string())));
    fields
}

fn ice_fields(cand: &str, mid: Option<&str>, mlineidx: Option<u32>) -> Vec<(String, Json)> {
    let mut fields = alloc::vec![("cand".to_string(), Json::Str(cand.to_string()))];
    opt_field(&mut fields, "mid", mid.map(|m| Json::Str(m.to_string())));
    opt_field(
        &mut fields,
        "mlineidx",
        mlineidx.map(|n| Json::Num(i64::from(n))),
    );
    fields
}

fn presence_fields(
    member: &str,
    state: PresenceState,
    kind: Option<ToggleKind>,
    role: Option<Role>,
) -> Vec<(String, Json)> {
    let mut fields = alloc::vec![
        ("member".to_string(), Json::Str(member.to_string())),
        ("state".to_string(), Json::Str(state.wire().to_string())),
    ];
    opt_field(
        &mut fields,
        "kind",
        kind.map(|k| Json::Str(k.wire().to_string())),
    );
    opt_field(
        &mut fields,
        "role",
        role.map(|r| Json::Str(r.wire().to_string())),
    );
    fields
}

fn offer_fields(
    sdp: &str,
    tracks: Option<&[TrackHint]>,
    chunk: Option<&ChunkMeta>,
) -> Vec<(String, Json)> {
    let mut fields = alloc::vec![("sdp".to_string(), Json::Str(sdp.to_string()))];
    opt_field(
        &mut fields,
        "tracks",
        tracks.map(|t| Json::Array(t.iter().map(TrackHint::to_json).collect())),
    );
    if let Some(chunk) = chunk {
        fields.extend(chunk_meta_fields(chunk));
    }
    fields
}

fn answer_fields(sdp: &str, chunk: Option<&ChunkMeta>) -> Vec<(String, Json)> {
    let mut fields = alloc::vec![("sdp".to_string(), Json::Str(sdp.to_string()))];
    if let Some(chunk) = chunk {
        fields.extend(chunk_meta_fields(chunk));
    }
    fields
}

fn joined_fields(
    members: &[String],
    mode: Option<RoomKind>,
    role: Option<Role>,
    streamers: Option<&[String]>,
    turn: Option<&TurnCredentials>,
    tracks: Option<&[TrackHint]>,
) -> Vec<(String, Json)> {
    let mut fields = alloc::vec![("members".to_string(), strings_json(members))];
    opt_field(
        &mut fields,
        "mode",
        mode.map(|m| Json::Str(m.wire().to_string())),
    );
    opt_field(
        &mut fields,
        "role",
        role.map(|r| Json::Str(r.wire().to_string())),
    );
    opt_field(&mut fields, "streamers", streamers.map(strings_json));
    opt_field(&mut fields, "turn", turn.map(TurnCredentials::to_json));
    opt_field(
        &mut fields,
        "tracks",
        tracks.map(|t| Json::Array(t.iter().map(TrackHint::to_json).collect())),
    );
    fields
}

fn error_fields(error: Option<&str>) -> Vec<(String, Json)> {
    let mut fields = Vec::new();
    opt_field(
        &mut fields,
        "error",
        error.map(|e| Json::Str(e.to_string())),
    );
    fields
}

fn decode_on_off(value: &Json) -> Option<OnOff> {
    OnOff::parse(&field_string(value, "state")?)
}

fn decode_joined(value: &Json) -> Option<Signal> {
    let members = strings(value.field("members")?)?;
    let mode = field_string_opt(value, "mode").and_then(|s| RoomKind::parse(&s));
    let role = field_string_opt(value, "role").and_then(|s| Role::parse(&s));
    let streamers = value.field("streamers").and_then(strings);
    let turn = value.field("turn").and_then(TurnCredentials::from_json);
    let tracks = value.field("tracks").and_then(decode_tracks);
    Some(Signal::Joined {
        members,
        mode,
        role,
        streamers,
        turn,
        tracks,
    })
}

fn decode_tracks(value: &Json) -> Option<Vec<TrackHint>> {
    value.as_array()?.iter().map(TrackHint::from_json).collect()
}

fn decode_offer(value: &Json) -> Option<Signal> {
    Some(Signal::Offer {
        sdp: field_string(value, "sdp")?,
        tracks: value.field("tracks").and_then(decode_tracks),
        chunk: decode_chunk(value),
    })
}

fn decode_answer(value: &Json) -> Option<Signal> {
    Some(Signal::Answer {
        sdp: field_string(value, "sdp")?,
        chunk: decode_chunk(value),
    })
}

fn decode_ice(value: &Json) -> Option<Signal> {
    Some(Signal::Ice {
        cand: field_string(value, "cand")?,
        mid: field_string_opt(value, "mid"),
        mlineidx: value.field("mlineidx").and_then(Json::as_u32),
    })
}

fn decode_presence(value: &Json) -> Option<Signal> {
    Some(Signal::Presence {
        member: field_string(value, "member")?,
        state: PresenceState::parse(&field_string(value, "state")?)?,
        kind: field_string_opt(value, "kind").and_then(|s| ToggleKind::parse(&s)),
        role: field_string_opt(value, "role").and_then(|s| Role::parse(&s)),
    })
}

fn decode_react(value: &Json) -> Option<Signal> {
    Some(Signal::React {
        member: field_string_opt(value, "member"),
        emoji: field_string(value, "emoji")?,
    })
}

fn decode_role(value: &Json) -> Option<Signal> {
    Some(Signal::Role {
        member: field_string(value, "member")?,
        role: Role::parse(&field_string(value, "role")?)?,
    })
}

// ---------------------------------------------------------------------------------------------
// SDP chunking: splitting an oversized offer/answer, and reassembling one from its chunks.
// ---------------------------------------------------------------------------------------------

/// Default per-chunk SDP budget, in bytes.
///
/// IRCv3 specifies a 4094-byte client tag-value ceiling, and ObbyIRCd raises the limit its relay
/// actually carries to 8191. A host on a different
/// ircd, or one that wants headroom for the JSON and tag-value escaping this module does not
/// itself perform, should measure its own server's real budget and pass that instead of trusting
/// either number blindly.
pub const DEFAULT_CHUNK_BUDGET: usize = 8191;

/// One numbered slice of a split `offer`/`answer` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct SdpChunk {
    /// This slice's correlation fields.
    pub chunk: ChunkMeta,
    /// The slice of the sdp this chunk carries.
    pub sdp: String,
}

/// Split `sdp` into chunks of at most `budget` bytes each, sharing `id`.
///
/// Returns `None` when `sdp` already fits within `budget`, so the caller sends it as a plain
/// unchunked `offer`/`answer` rather than a chunk of one.
pub fn split_sdp(sdp: &str, id: &str, budget: usize) -> Option<Vec<SdpChunk>> {
    if budget == 0 || sdp.len() <= budget {
        return None;
    }
    let slices = char_chunks(sdp, budget)?;
    let total = u32::try_from(slices.len()).unwrap_or(u32::MAX);
    Some(
        slices
            .into_iter()
            .enumerate()
            .map(|(seq, slice)| SdpChunk {
                chunk: ChunkMeta {
                    id: id.to_string(),
                    seq: u32::try_from(seq).unwrap_or(u32::MAX),
                    total,
                },
                sdp: slice.to_string(),
            })
            .collect(),
    )
}

/// Cut `s` into pieces of at most `budget` bytes each, never splitting a UTF-8 character.
///
/// A character wider than `budget` still gets a whole chunk to itself: rounding `end` down to
/// the nearest boundary can walk it all the way back to `start`, and a chunk cannot be empty
/// without stalling `start` forever, so that case rounds `end` up to the next boundary instead.
fn char_chunks(s: &str, budget: usize) -> Option<Vec<&str>> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < s.len() {
        let mut end = (start + budget).min(s.len());
        while end > start && !s.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            end = start + 1;
            while end < s.len() && !s.is_char_boundary(end) {
                end += 1;
            }
        }
        chunks.push(s.get(start..end)?);
        start = end;
    }
    Some(chunks)
}

/// How many chunks one in-flight reassembly may claim before it is refused outright.
///
/// A peer naming an implausibly large `total` would otherwise have this buffer chunks forever
/// waiting for pieces that may never arrive.
pub const DEFAULT_MAX_CHUNKS_PER_REASSEMBLY: usize = 64;

/// How many distinct chunked signals may be reassembling at once.
///
/// A peer opening an unbounded number of `id`s, each fed only a fraction of its chunks, would
/// otherwise grow this connection's memory without limit.
pub const DEFAULT_MAX_CONCURRENT_REASSEMBLIES: usize = 16;

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
struct PartialSdp {
    total: u32,
    parts: BTreeMap<u32, String>,
}

/// A bounded buffer that reassembles `offer`/`answer` frames split across chunks.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct SdpReassembler {
    partials: BTreeMap<String, PartialSdp>,
    max_chunks: usize,
    max_concurrent: usize,
}

impl SdpReassembler {
    /// A reassembler using the default bounds.
    pub fn new() -> Self {
        Self::with_bounds(
            DEFAULT_MAX_CHUNKS_PER_REASSEMBLY,
            DEFAULT_MAX_CONCURRENT_REASSEMBLIES,
        )
    }

    /// A reassembler bounded to `max_chunks` per id, and `max_concurrent` ids in flight at once.
    pub fn with_bounds(max_chunks: usize, max_concurrent: usize) -> Self {
        Self {
            partials: BTreeMap::new(),
            max_chunks,
            max_concurrent,
        }
    }

    /// Feed one chunk, returning the joined sdp once every piece named by `chunk.total` has
    /// arrived, in `seq` order regardless of the order they arrived in.
    pub fn push(&mut self, chunk: &ChunkMeta, sdp: &str) -> Option<String> {
        let total = usize::try_from(chunk.total).unwrap_or(usize::MAX);
        if chunk.total == 0 || total > self.max_chunks || chunk.seq >= chunk.total {
            return None;
        }
        if !self.partials.contains_key(&chunk.id) && self.partials.len() >= self.max_concurrent {
            return None;
        }
        let partial = self
            .partials
            .entry(chunk.id.clone())
            .or_insert_with(|| PartialSdp {
                total: chunk.total,
                parts: BTreeMap::new(),
            });
        if partial.total != chunk.total {
            return None;
        }
        partial.parts.insert(chunk.seq, sdp.to_string());
        if partial.parts.len() != total {
            return None;
        }
        let complete = self.partials.remove(&chunk.id)?;
        let mut joined = String::new();
        for seq in 0..complete.total {
            joined.push_str(complete.parts.get(&seq)?);
        }
        Some(joined)
    }

    /// How many distinct ids are currently mid-reassembly.
    pub fn pending_len(&self) -> usize {
        self.partials.len()
    }

    /// Discard every in-flight reassembly, for a connection that just dropped.
    pub fn drop_all(&mut self) {
        self.partials.clear();
    }
}

impl Default for SdpReassembler {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------------
// Room state.
// ---------------------------------------------------------------------------------------------

/// One participant's state within a [`Room`].
///
/// Every toggle is [`OnOff`] rather than `bool`: this is the same distinction clippy's own
/// `struct_excessive_bools` lint asks for (six independent flags read equally well as a state
/// machine's cases), and reusing `OnOff` rather than inventing six near-identical two-variant
/// enums keeps it to one type.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "VoiceParticipant"))]
pub struct Participant {
    /// Their nick as the server spells it, since the map that holds them is keyed by a fold that
    /// throws that spelling away.
    pub nick: String,
    /// Whether they may publish, decided by the room kind and, in a `$` room, whether the
    /// server named them a streamer.
    pub role: Role,
    /// Microphone on.
    pub mic: OnOff,
    /// Camera on.
    pub video: OnOff,
    /// The voice-activity flag the last `presence` reported.
    pub speaking: OnOff,
    /// Not receiving room audio.
    pub deaf: OnOff,
    /// Screen share active.
    pub screen: OnOff,
    /// Hand raised.
    pub hand: OnOff,
}

impl Participant {
    /// Someone in a room with every toggle off, in the role a room hands out by default.
    pub fn new(nick: impl Into<String>) -> Self {
        Self {
            nick: nick.into(),
            ..Self::default()
        }
    }
}

impl Default for Participant {
    fn default() -> Self {
        Self {
            nick: String::new(),
            role: Role::Viewer,
            mic: OnOff::Off,
            video: OnOff::Off,
            speaking: OnOff::Off,
            deaf: OnOff::Off,
            screen: OnOff::Off,
            hand: OnOff::Off,
        }
    }
}

/// The state of one voice room: who is in it, their kind of channel, and every participant's
/// mic, video, speaking, deaf, screen and hand state and role.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "VoiceRoom"))]
pub struct Room {
    /// The channel this room's signalling is scoped to.
    pub channel: String,
    /// Whether every member publishes, or only the streamer and whoever they promote.
    pub kind: RoomKind,
    /// Every known participant, keyed by their folded nick.
    pub participants: BTreeMap<CaseFolded, Participant>,
    /// The TURN credentials the SFU handed us on `joined`, if it has yet.
    pub turn: Option<TurnCredentials>,
}

impl Room {
    /// A room for `channel` with no participants yet, whose kind follows the `^`/`$` prefix.
    pub fn new(channel: impl Into<String>) -> Self {
        let channel = channel.into();
        let kind = RoomKind::for_channel(&channel);
        Self {
            channel,
            kind,
            participants: BTreeMap::new(),
            turn: None,
        }
    }

    /// Apply one server signal, updating membership and per-participant state.
    ///
    /// `me` is our own nick, needed to resolve the local role a `joined` frame reports for us
    /// alone. `casemap` is the server's, because two spellings of one nick are one person here
    /// exactly as they are everywhere else. Every other frame type is ignored: they are either
    /// outbound intents this room waits to see echoed back as `presence`, or carry nothing about
    /// membership or state.
    pub fn apply(&mut self, me: &str, signal: &Signal, casemap: Casemapping) {
        match signal {
            Signal::Joined {
                members,
                role,
                streamers,
                turn,
                ..
            } => self.apply_joined(
                me,
                members,
                *role,
                streamers.as_deref(),
                turn.as_ref(),
                casemap,
            ),
            Signal::Presence {
                member,
                state,
                kind,
                role,
            } => self.apply_presence(member, *state, *kind, *role, casemap),
            Signal::Role { member, role } => {
                self.participant_mut(member, casemap).role = *role;
            }
            _ => {}
        }
    }

    fn apply_joined(
        &mut self,
        me: &str,
        members: &[String],
        role: Option<Role>,
        streamers: Option<&[String]>,
        turn: Option<&TurnCredentials>,
        casemap: Casemapping,
    ) {
        self.turn = turn.cloned();
        for member in members {
            let assigned = match (casemap.eq(member, me), role) {
                (true, Some(role)) => role,
                _ => self.role_for(member, streamers, casemap),
            };
            self.participant_mut(member, casemap).role = assigned;
        }
    }

    /// The participant record for a nick, created in the room's default role if it is new.
    fn participant_mut(&mut self, member: &str, casemap: Casemapping) -> &mut Participant {
        self.participants
            .entry(casemap.fold(member))
            .or_insert_with(|| Participant::new(member))
    }

    fn apply_presence(
        &mut self,
        member: &str,
        state: PresenceState,
        kind: Option<ToggleKind>,
        role: Option<Role>,
        casemap: Casemapping,
    ) {
        match state {
            PresenceState::Joined => {
                let assigned = role.unwrap_or_else(|| self.role_for(member, None, casemap));
                self.participant_mut(member, casemap).role = assigned;
            }
            PresenceState::Left => {
                self.participants.remove(&casemap.fold(member));
            }
            PresenceState::On | PresenceState::Off => {
                if let (Some(kind), Some(participant)) =
                    (kind, self.participants.get_mut(&casemap.fold(member)))
                {
                    let toggled = if state == PresenceState::On {
                        OnOff::On
                    } else {
                        OnOff::Off
                    };
                    kind.apply(participant, toggled);
                }
            }
            PresenceState::Speaking => {
                self.set_flag(member, casemap, |p| p.speaking = OnOff::On);
            }
            PresenceState::Silent => self.set_flag(member, casemap, |p| p.speaking = OnOff::Off),
            PresenceState::DeafOn => self.set_flag(member, casemap, |p| p.deaf = OnOff::On),
            PresenceState::DeafOff => self.set_flag(member, casemap, |p| p.deaf = OnOff::Off),
        }
    }

    fn set_flag(&mut self, member: &str, casemap: Casemapping, f: impl FnOnce(&mut Participant)) {
        if let Some(participant) = self.participants.get_mut(&casemap.fold(member)) {
            f(participant);
        }
    }

    fn role_for(&self, member: &str, streamers: Option<&[String]>, casemap: Casemapping) -> Role {
        match self.kind {
            RoomKind::Publish => Role::Publisher,
            RoomKind::Stream => {
                let is_streamer =
                    streamers.is_some_and(|list| list.iter().any(|name| casemap.eq(name, member)));
                if is_streamer {
                    Role::Publisher
                } else {
                    Role::Viewer
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn() -> TurnCredentials {
        TurnCredentials {
            urls: alloc::vec!["turn:relay.obby.chat:3478".to_string()],
            username: "user1".to_string(),
            password: "pass1".to_string(),
        }
    }

    #[test]
    fn join_and_leave_round_trip_with_only_a_channel() {
        let join = Signal::Join {
            channel: "#general".to_string(),
        };
        assert_eq!(join.to_json(), r##"{"type":"join","channel":"#general"}"##);
        assert_eq!(Signal::from_json(&join.to_json()), Some(join));

        let leave = Signal::Leave {
            channel: "^voice".to_string(),
        };
        assert_eq!(leave.to_json(), r#"{"type":"leave","channel":"^voice"}"#);
        assert_eq!(Signal::from_json(&leave.to_json()), Some(leave));
    }

    #[test]
    fn joined_round_trips_with_every_optional_field_present() {
        let joined = Signal::Joined {
            members: alloc::vec!["alice".to_string(), "bob".to_string()],
            mode: Some(RoomKind::Stream),
            role: Some(Role::Publisher),
            streamers: Some(alloc::vec!["alice".to_string()]),
            turn: Some(turn()),
            tracks: Some(alloc::vec![TrackHint {
                mid: "0".to_string(),
                member: "alice".to_string(),
            }]),
        };
        let json = joined.to_json();
        assert!(json.starts_with(r#"{"type":"joined","members":["alice","bob"]"#));
        assert!(json.contains(r#""mode":"stream""#));
        assert!(json.contains(r#""role":"streamer""#));
        assert!(json.contains(r#""streamers":["alice"]"#));
        assert!(json.contains(
            r#""turn":{"urls":["turn:relay.obby.chat:3478"],"username":"user1","password":"pass1"}"#
        ));
        assert!(json.contains(r#""tracks":[{"mid":"0","member":"alice"}]"#));
        assert_eq!(Signal::from_json(&json), Some(joined));
    }

    #[test]
    fn joined_round_trips_with_every_optional_field_absent() {
        let joined = Signal::Joined {
            members: alloc::vec!["alice".to_string()],
            mode: None,
            role: None,
            streamers: None,
            turn: None,
            tracks: None,
        };
        assert_eq!(joined.to_json(), r#"{"type":"joined","members":["alice"]}"#);
        assert_eq!(Signal::from_json(&joined.to_json()), Some(joined));
    }

    #[test]
    fn offer_and_answer_round_trip_unchunked() {
        let offer = Signal::Offer {
            sdp: "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n".to_string(),
            tracks: None,
            chunk: None,
        };
        assert_eq!(Signal::from_json(&offer.to_json()), Some(offer.clone()));
        assert!(!offer.to_json().contains("\"id\""));

        let answer = Signal::Answer {
            sdp: "v=0\r\n".to_string(),
            chunk: None,
        };
        assert_eq!(Signal::from_json(&answer.to_json()), Some(answer));
    }

    #[test]
    fn offer_and_answer_round_trip_with_chunk_metadata() {
        let offer = Signal::Offer {
            sdp: "partial-sdp".to_string(),
            tracks: None,
            chunk: Some(ChunkMeta {
                id: "abc123".to_string(),
                seq: 1,
                total: 3,
            }),
        };
        let json = offer.to_json();
        assert_eq!(
            json,
            r#"{"type":"offer","sdp":"partial-sdp","id":"abc123","seq":1,"total":3}"#
        );
        assert_eq!(Signal::from_json(&json), Some(offer));
    }

    #[test]
    fn ice_round_trips_with_and_without_mid() {
        let with_mid = Signal::Ice {
            cand: "candidate:1 1 UDP 1 1.2.3.4 5 typ host".to_string(),
            mid: Some("0".to_string()),
            mlineidx: Some(0),
        };
        assert_eq!(Signal::from_json(&with_mid.to_json()), Some(with_mid));

        let without_mid = Signal::Ice {
            cand: "candidate:1 1 UDP 1 1.2.3.4 5 typ host".to_string(),
            mid: None,
            mlineidx: None,
        };
        assert_eq!(
            without_mid.to_json(),
            r#"{"type":"ice","cand":"candidate:1 1 UDP 1 1.2.3.4 5 typ host"}"#
        );
        assert_eq!(Signal::from_json(&without_mid.to_json()), Some(without_mid));
    }

    #[test]
    fn presence_round_trips_all_three_shapes() {
        let membership = Signal::Presence {
            member: "bob".to_string(),
            state: PresenceState::Joined,
            kind: None,
            role: Some(Role::Viewer),
        };
        assert_eq!(
            membership.to_json(),
            r#"{"type":"presence","member":"bob","state":"joined","role":"viewer"}"#
        );
        assert_eq!(Signal::from_json(&membership.to_json()), Some(membership));

        let toggle = Signal::Presence {
            member: "bob".to_string(),
            state: PresenceState::On,
            kind: Some(ToggleKind::Mic),
            role: None,
        };
        assert_eq!(
            toggle.to_json(),
            r#"{"type":"presence","member":"bob","state":"on","kind":"mic"}"#
        );
        assert_eq!(Signal::from_json(&toggle.to_json()), Some(toggle));

        let activity = Signal::Presence {
            member: "bob".to_string(),
            state: PresenceState::DeafOn,
            kind: None,
            role: None,
        };
        assert_eq!(
            activity.to_json(),
            r#"{"type":"presence","member":"bob","state":"deaf-on"}"#
        );
        assert_eq!(Signal::from_json(&activity.to_json()), Some(activity));
    }

    #[test]
    fn the_six_toggle_intents_round_trip() {
        let frames = [
            Signal::Mic { state: OnOff::On },
            Signal::Video { state: OnOff::Off },
            Signal::Screen { state: OnOff::On },
            Signal::Hand { state: OnOff::On },
            Signal::Speaking,
            Signal::Silent,
        ];
        for frame in frames {
            assert_eq!(Signal::from_json(&frame.to_json()), Some(frame));
        }
    }

    #[test]
    fn deaf_is_a_real_outbound_frame_not_only_a_local_flag() {
        let deafen = Signal::Deaf { state: OnOff::On };
        assert_eq!(deafen.to_json(), r#"{"type":"deaf","state":"on"}"#);
        assert_eq!(Signal::from_json(&deafen.to_json()), Some(deafen));
    }

    #[test]
    fn react_round_trips_outbound_and_inbound_shapes() {
        let outbound = Signal::React {
            member: None,
            emoji: "👍".to_string(),
        };
        assert_eq!(outbound.to_json(), r#"{"type":"react","emoji":"👍"}"#);
        assert_eq!(Signal::from_json(&outbound.to_json()), Some(outbound));

        let inbound = Signal::React {
            member: Some("carol".to_string()),
            emoji: "👍".to_string(),
        };
        assert_eq!(
            inbound.to_json(),
            r#"{"type":"react","member":"carol","emoji":"👍"}"#
        );
        assert_eq!(Signal::from_json(&inbound.to_json()), Some(inbound));
    }

    #[test]
    fn promote_demote_and_role_round_trip() {
        let promote = Signal::Promote {
            target: "bob".to_string(),
        };
        assert_eq!(Signal::from_json(&promote.to_json()), Some(promote));

        let demote = Signal::Demote {
            target: "bob".to_string(),
        };
        assert_eq!(Signal::from_json(&demote.to_json()), Some(demote));

        let role = Signal::Role {
            member: "bob".to_string(),
            role: Role::Publisher,
        };
        assert_eq!(
            role.to_json(),
            r#"{"type":"role","member":"bob","role":"streamer"}"#
        );
        assert_eq!(Signal::from_json(&role.to_json()), Some(role));
    }

    #[test]
    fn error_round_trips_with_and_without_a_reason() {
        let with_reason = Signal::Error {
            error: Some("room is full".to_string()),
        };
        assert_eq!(
            with_reason.to_json(),
            r#"{"type":"error","error":"room is full"}"#
        );
        assert_eq!(Signal::from_json(&with_reason.to_json()), Some(with_reason));

        let without_reason = Signal::Error { error: None };
        assert_eq!(without_reason.to_json(), r#"{"type":"error"}"#);
        assert_eq!(
            Signal::from_json(&without_reason.to_json()),
            Some(without_reason)
        );
    }

    #[test]
    fn unknown_frame_types_decode_to_none() {
        assert_eq!(Signal::from_json(r#"{"type":"nonsense"}"#), None);
    }

    #[test]
    fn malformed_json_decodes_to_none() {
        assert_eq!(Signal::from_json("not json"), None);
        assert_eq!(Signal::from_json(r#"{"type":"join""#), None);
        assert_eq!(Signal::from_json(r##"{"channel":"#general"}"##), None);
    }

    #[test]
    fn an_sdp_within_budget_is_not_split() {
        assert_eq!(split_sdp("short sdp", "id1", 8191), None);
    }

    #[test]
    fn an_oversized_sdp_is_split_into_numbered_chunks_sharing_one_id() {
        let sdp = "abcdefghij";
        let chunks = split_sdp(sdp, "call-1", 3).expect("must split");
        assert_eq!(chunks.len(), 4);
        for (seq, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.chunk.id, "call-1");
            assert_eq!(chunk.chunk.seq, u32::try_from(seq).unwrap_or(u32::MAX));
            assert_eq!(chunk.chunk.total, 4);
        }
        let joined: String = chunks.iter().map(|c| c.sdp.as_str()).collect();
        assert_eq!(joined, sdp);
    }

    #[test]
    fn a_multibyte_character_is_never_split_across_a_chunk_boundary() {
        let sdp = "a👍b";
        let chunks = split_sdp(sdp, "id", 2).expect("must split");
        for chunk in &chunks {
            assert!(core::str::from_utf8(chunk.sdp.as_bytes()).is_ok());
        }
        let joined: String = chunks.iter().map(|c| c.sdp.as_str()).collect();
        assert_eq!(joined, sdp);
    }

    #[test]
    fn chunks_reassemble_regardless_of_arrival_order() {
        let chunks = split_sdp(&"x".repeat(20), "call-1", 6).expect("must split");
        let mut reassembler = SdpReassembler::new();
        let mut out = None;
        for chunk in chunks.iter().rev() {
            out = reassembler.push(&chunk.chunk, &chunk.sdp);
        }
        assert_eq!(out, Some("x".repeat(20)));
        assert_eq!(reassembler.pending_len(), 0);
    }

    #[test]
    fn a_reassembly_missing_a_chunk_never_completes() {
        let chunks = split_sdp(&"y".repeat(20), "call-2", 6).expect("must split");
        let mut reassembler = SdpReassembler::new();
        for chunk in chunks.iter().take(chunks.len() - 1) {
            assert_eq!(reassembler.push(&chunk.chunk, &chunk.sdp), None);
        }
        assert_eq!(reassembler.pending_len(), 1);
    }

    #[test]
    fn reassembly_rejects_a_total_over_the_configured_bound() {
        let mut reassembler = SdpReassembler::with_bounds(4, 16);
        let chunk = ChunkMeta {
            id: "huge".to_string(),
            seq: 0,
            total: 5,
        };
        assert_eq!(reassembler.push(&chunk, "slice"), None);
        assert_eq!(reassembler.pending_len(), 0);
    }

    #[test]
    fn reassembly_bounds_the_number_of_concurrent_ids() {
        let mut reassembler = SdpReassembler::with_bounds(64, 2);
        for id in ["a", "b"] {
            let chunk = ChunkMeta {
                id: id.to_string(),
                seq: 0,
                total: 2,
            };
            assert_eq!(reassembler.push(&chunk, "part"), None);
        }
        assert_eq!(reassembler.pending_len(), 2);

        let overflow = ChunkMeta {
            id: "c".to_string(),
            seq: 0,
            total: 2,
        };
        assert_eq!(reassembler.push(&overflow, "part"), None);
        assert_eq!(
            reassembler.pending_len(),
            2,
            "a third id must not grow past the configured bound"
        );
    }

    #[test]
    fn dropping_all_forgets_every_partial_reassembly() {
        let mut reassembler = SdpReassembler::new();
        let chunk = ChunkMeta {
            id: "call".to_string(),
            seq: 0,
            total: 2,
        };
        reassembler.push(&chunk, "part");
        reassembler.drop_all();
        assert_eq!(reassembler.pending_len(), 0);
    }

    #[test]
    fn every_member_publishes_in_a_publish_room() {
        let mut room = Room::new("^voice");
        assert_eq!(room.kind, RoomKind::Publish);
        room.apply(
            "me",
            &Signal::Joined {
                members: alloc::vec!["me".to_string(), "bob".to_string()],
                mode: None,
                role: None,
                streamers: None,
                turn: None,
                tracks: None,
            },
            Casemapping::Rfc1459,
        );
        for nick in ["me", "bob"] {
            assert_eq!(
                room.participants[&CaseFolded::from(nick)].role,
                Role::Publisher
            );
        }
    }

    #[test]
    fn only_the_streamers_publish_in_a_stream_room() {
        let mut room = Room::new("$live");
        assert_eq!(room.kind, RoomKind::Stream);
        room.apply(
            "me",
            &Signal::Joined {
                members: alloc::vec!["alice".to_string(), "me".to_string()],
                mode: Some(RoomKind::Stream),
                role: Some(Role::Viewer),
                streamers: Some(alloc::vec!["alice".to_string()]),
                turn: None,
                tracks: None,
            },
            Casemapping::Rfc1459,
        );
        assert_eq!(
            room.participants[&CaseFolded::from("alice")].role,
            Role::Publisher
        );
        assert_eq!(
            room.participants[&CaseFolded::from("me")].role,
            Role::Viewer
        );
    }

    #[test]
    fn a_promote_broadcast_updates_the_targets_role() {
        let mut room = Room::new("$live");
        room.apply(
            "me",
            &Signal::Role {
                member: "bob".to_string(),
                role: Role::Publisher,
            },
            Casemapping::Rfc1459,
        );
        assert_eq!(
            room.participants[&CaseFolded::from("bob")].role,
            Role::Publisher
        );
    }

    #[test]
    fn presence_toggles_update_the_matching_participant_flag() {
        let mut room = Room::new("^voice");
        room.participants
            .insert(CaseFolded::from("bob"), Participant::new("bob"));
        room.apply(
            "me",
            &Signal::Presence {
                member: "bob".to_string(),
                state: PresenceState::On,
                kind: Some(ToggleKind::Video),
                role: None,
            },
            Casemapping::Rfc1459,
        );
        assert_eq!(room.participants[&CaseFolded::from("bob")].video, OnOff::On);
        assert_eq!(room.participants[&CaseFolded::from("bob")].mic, OnOff::Off);
    }

    #[test]
    fn one_participant_under_two_spellings_is_one_person() {
        let mut room = Room::new("^voice");
        room.apply(
            "me",
            &Signal::Presence {
                member: "Bob[dev]".to_string(),
                state: PresenceState::Joined,
                kind: None,
                role: None,
            },
            Casemapping::Rfc1459,
        );
        room.apply(
            "me",
            &Signal::Presence {
                member: "bob{dev}".to_string(),
                state: PresenceState::On,
                kind: Some(ToggleKind::Mic),
                role: None,
            },
            Casemapping::Rfc1459,
        );
        assert_eq!(room.participants.len(), 1);
        let participant = room
            .participants
            .values()
            .next()
            .expect("the room has one participant");
        assert_eq!(participant.nick, "Bob[dev]");
        assert_eq!(participant.mic, OnOff::On);
    }

    #[test]
    fn presence_joined_and_left_add_and_remove_participants() {
        let mut room = Room::new("^voice");
        room.apply(
            "me",
            &Signal::Presence {
                member: "bob".to_string(),
                state: PresenceState::Joined,
                kind: None,
                role: None,
            },
            Casemapping::Rfc1459,
        );
        assert!(room.participants.contains_key(&CaseFolded::from("bob")));

        room.apply(
            "me",
            &Signal::Presence {
                member: "bob".to_string(),
                state: PresenceState::Left,
                kind: None,
                role: None,
            },
            Casemapping::Rfc1459,
        );
        assert!(!room.participants.contains_key(&CaseFolded::from("bob")));
    }

    #[test]
    fn deafen_broadcasts_to_the_room_instead_of_staying_local() {
        let mut room = Room::new("^voice");
        room.participants
            .insert(CaseFolded::from("bob"), Participant::new("bob"));
        room.apply(
            "me",
            &Signal::Presence {
                member: "bob".to_string(),
                state: PresenceState::DeafOn,
                kind: None,
                role: None,
            },
            Casemapping::Rfc1459,
        );
        assert_eq!(
            room.participants[&CaseFolded::from("bob")].deaf,
            OnOff::On,
            "the room's model must learn a peer's deafen from presence, the way it learns mic or video"
        );

        let intent = Signal::Deaf { state: OnOff::On };
        assert!(
            intent.to_json().contains("\"type\":\"deaf\""),
            "deafening must produce a real outbound frame, not only flip a local field"
        );
    }
}
