//! IRCv3 wire format.
//!
//! This crate holds no connection and no client state. It turns bytes into a [`Message`] and back,
//! and it knows the rules that depend only on the line itself: tag escaping, casemapping, ISUPPORT
//! token grammar, mode argument arity.
//!
//! ```
//! use obby_proto::Message;
//!
//! let msg = Message::parse("@time=2026-09-06T10:00:00.000Z :nick!u@h PRIVMSG #chan :hello")?;
//! assert_eq!(msg.command, "PRIVMSG");
//! assert_eq!(msg.params, ["#chan", "hello"]);
//! assert_eq!(msg.tag("time"), Some("2026-09-06T10:00:00.000Z"));
//! # Ok::<(), obby_proto::ParseError>(())
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod casemap;
mod format;
mod isupport;
mod message;
mod mode;
mod servertime;
mod tags;

pub use casemap::{CaseFolded, Casemapping};
pub use format::{Colour, Ctcp, Emphasis, Span, Style, parse_ctcp, parse_spans, strip_formatting};
pub use isupport::{ChanModes, Isupport, Prefix, Token};
pub use message::{Message, ParseError, Source};
pub use mode::{ModeChange, parse_channel_modes, parse_user_modes};
pub use servertime::{format as format_server_time, parse as parse_server_time};
pub use tags::{Tag, Tags};

/// The byte budget for everything after the tags, including the trailing CRLF.
///
/// RFC 1459 sets this and IRCv3 leaves it alone; only the tag section grew.
pub const MAX_LINE_BYTES: usize = 512;

/// The byte budget for the tag section, excluding the leading `@` and the separating space.
///
/// The `message-tags` capability raises the total line budget by this much.
pub const MAX_TAG_BYTES: usize = 8191;

/// The byte budget a client may spend on its own `+`-prefixed tags.
///
/// A server may raise this, and no ISUPPORT token advertises it, so a client cannot discover the
/// real ceiling and has to be told. ObbyIRCd allows 8191 for general client tags so one TAGMSG can
/// carry a whole escaped WebRTC SDP, while holding the e2ee tag to this value on the same
/// connection. Treat this as the floor to assume, not the limit to use.
pub const MAX_CLIENT_TAG_BYTES: usize = 4094;
