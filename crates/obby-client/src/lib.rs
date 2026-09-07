//! The Obby client engine.
//!
//! One [`Client`] is one connection. It owns the registration state machine, the negotiated
//! capabilities and the client model. It opens no socket, reads no clock and renders nothing: the
//! host pushes bytes and time in, and drains bytes and events out.
//!
//! ```
//! use obby_client::{Client, Config, Event};
//!
//! let mut client = Client::new(Config::new("mynick"));
//! client.handle_connected();
//!
//! // whatever the host wrote is now waiting to go on the wire
//! let first = client.poll_transmit().expect("registration starts on connect");
//! assert!(first.starts_with(b"CAP LS 302"));
//!
//! client.handle_bytes(b":irc.example.org CAP * LS :sasl multi-prefix\r\n");
//! client.handle_bytes(b":irc.example.org CAP * ACK :multi-prefix\r\n");
//! assert!(matches!(client.poll_event(), Some(Event::CapAcknowledged { .. })));
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod batch;
mod caps;
mod client;
mod command;
#[cfg(feature = "e2ee")]
mod e2ee;
#[cfg(feature = "obby")]
mod extensions;
mod label;
mod model;
mod monitor;
mod sasl;
mod scram;
mod session;
mod timer;
#[cfg(feature = "voice")]
mod voice;

pub use caps::{Capability, Caps, WANTED_CAPS};
pub use client::{Client, Config, Event, Phase, Severity};
pub use command::{Command, Typing};
#[cfg(feature = "e2ee")]
pub use e2ee::{
    Error as E2eeError, Fingerprint, Frag, Frame, HandshakeResponse, Identity, IdentityPublic,
    MAX_SKIP, MAX_SKIPPED_KEYS, PROTOCOL_VERSION as E2EE_PROTOCOL_VERSION, PeerTrust, PendingOffer,
    PinOutcome, PreKeyBundle, RandomSource, Ratchet, RatchetMessage, Role as E2eeRole, Session,
    SessionState, accept_offer, complete_handshake, create_offer, keeps_own_offer, reassemble,
};
#[cfg(feature = "obby")]
pub use extensions::{
    Bot, BotCommand, Bots, Commands, Invitation, LinkPreview, PRIVILEGED_COMMANDS, is_privileged,
};
pub use model::{
    Channel, Conversation, DEFAULT_RETENTION, Log, Me, Membership, Message, MessageKey,
    MessageKind, Model, Person,
};
pub use monitor::Monitor;
pub use sasl::{Credentials, SaslFailure};
pub use scram::{Scram, ScramError};
pub use session::Change;
pub use timer::Now;
#[cfg(feature = "voice")]
pub use voice::{
    ChunkMeta, DEFAULT_CHUNK_BUDGET, DEFAULT_MAX_CHUNKS_PER_REASSEMBLY,
    DEFAULT_MAX_CONCURRENT_REASSEMBLIES, OnOff, Participant, PresenceState, Role, Room, RoomKind,
    SdpChunk, SdpReassembler, Signal, ToggleKind, TrackHint, TurnCredentials, split_sdp,
};

pub use obby_proto as proto;
