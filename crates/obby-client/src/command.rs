//! What a host asks the connection to do.
//!
//! One enum rather than a method per action, because this is the half of the API that crosses into
//! C, WASM, Python and Dart, and a single tagged value binds far more cheaply than a wide surface.

use alloc::string::String;

/// Whether we are still composing a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "TypingState"))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Typing {
    /// Typing right now.
    Active,
    /// Stopped, with text still in the box.
    Paused,
    /// Stopped, with the box empty.
    Done,
}

impl Typing {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Done => "done",
        }
    }
}

/// Something to do on this connection.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "serde", serde(tag = "type", rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Command {
    /// Join a channel.
    Join {
        /// The channel to join.
        channel: String,
        /// Its key, when it has one.
        key: Option<String>,
    },
    /// Leave a channel.
    Part {
        /// The channel to leave.
        channel: String,
        /// Why, shown to the others in it.
        reason: Option<String>,
    },
    /// Say something to a channel or a person.
    SendMessage {
        /// Where to say it.
        target: String,
        /// What to say.
        text: String,
    },
    /// Send a notice, which by convention must never be auto-replied to.
    SendNotice {
        /// Where to send it.
        target: String,
        /// What to send.
        text: String,
    },
    /// Send a `CTCP ACTION`, the third-person form.
    SendAction {
        /// Where to send it.
        target: String,
        /// What we are doing.
        text: String,
    },
    /// Change our nick.
    SetNick {
        /// The nick to take.
        nick: String,
    },
    /// Set or clear a channel topic.
    SetTopic {
        /// The channel.
        channel: String,
        /// The new topic, or nothing to clear it.
        topic: Option<String>,
    },
    /// Mark ourselves away, or come back.
    SetAway {
        /// The away message, or nothing to come back.
        message: Option<String>,
    },
    /// Say we are typing, so others can show it.
    SetTyping {
        /// Who we are typing to.
        target: String,
        /// How far along we are.
        state: Typing,
    },
    /// React to a message with an emoji.
    AddReaction {
        /// The channel or person the message is in.
        target: String,
        /// The message reacted to.
        msgid: String,
        /// The emoji.
        emoji: String,
    },
    /// Take a reaction back.
    RemoveReaction {
        /// The channel or person the message is in.
        target: String,
        /// The message.
        msgid: String,
        /// The emoji to remove.
        emoji: String,
    },
    /// Ask the server to delete a message.
    RedactMessage {
        /// Where the message is.
        target: String,
        /// The message to delete.
        msgid: String,
        /// Why, when the server wants a reason.
        reason: Option<String>,
    },
    /// Tell the server how far we have read.
    MarkRead {
        /// The channel or person.
        target: String,
        /// The `server-time` of the last message read.
        timestamp: String,
    },
    /// Ask for older messages than the ones we hold.
    ///
    /// With no `before`, this asks for the most recent, which is what a fresh window wants.
    FetchHistory {
        /// The channel or person.
        target: String,
        /// Fetch messages older than this one.
        before: Option<String>,
        /// How many to ask for.
        limit: u16,
    },
    /// Set one of our own metadata keys, or clear it.
    SetMetadata {
        /// The key, such as `display-name`, `color` or `avatar`.
        key: String,
        /// The value, or nothing to clear the key.
        value: Option<String>,
    },
    /// Ask to be told when these metadata keys change on anyone we can see.
    SubscribeMetadata {
        /// The keys to watch.
        keys: alloc::vec::Vec<String>,
    },
    /// Watch these nicks, so the server says when they come and go.
    WatchNicks {
        /// The nicks to watch.
        nicks: alloc::vec::Vec<String>,
    },
    /// Stop watching these nicks.
    UnwatchNicks {
        /// The nicks to stop watching.
        nicks: alloc::vec::Vec<String>,
    },
    /// Send a voice signalling frame to a room.
    ///
    /// The frame is the host's to build, because everything in it comes from the media stack the
    /// core deliberately knows nothing about.
    #[cfg(feature = "voice")]
    SendVoiceSignal {
        /// The channel to signal in.
        channel: String,
        /// The frame, already encoded as the JSON that travels in the tag.
        signal_json: String,
    },
    /// Leave the network.
    Quit {
        /// Why.
        reason: Option<String>,
    },
    /// Send a line we do not model. The escape hatch, so a host is never stuck waiting for us.
    SendRawLine {
        /// The line, without its terminator.
        line: String,
    },
}
