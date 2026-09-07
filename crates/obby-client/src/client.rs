//! The connection engine.

use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use obby_proto::{Casemapping, Isupport, Message};

use crate::batch::{Batches, ClosedBatch};
use crate::caps::Caps;
use crate::command::Command;
use crate::label::{DEFAULT_TIMEOUT_MS, Labels};
use crate::model::Model;
use crate::monitor::{self, Monitor};
use crate::sasl::{self, Credentials, SaslFailure, SaslState};
use crate::session::{self, Change};
use crate::timer::{
    DEAD_LINK_MS, Deadline, Now, PING_KEEPALIVE_MS, ReconnectBackoff, TYPING_EXPIRY_MS, Timers,
};

/// How far the connection has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[cfg_attr(feature = "ts", ts(rename = "ConnectionPhase"))]
pub enum Phase {
    /// No transport yet. Nothing has been written.
    Disconnected,
    /// The transport is up and capability negotiation is running.
    Negotiating,
    /// Capabilities are settled, `CAP END` and the registration lines are sent, waiting for `001`.
    Registering,
    /// `001` arrived. The connection is usable.
    Registered,
}

/// What a host needs to give the engine before it can connect.
///
/// Every field but the nick has a default, so a host deserialising one only has to supply what it
/// actually cares about. An empty username or realname is filled in from the nick.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct Config {
    /// The nick to register with.
    pub nick: String,
    /// The username sent in `USER`. Defaults to the nick.
    #[cfg_attr(feature = "ts", ts(as = "Option<String>", optional))]
    pub username: String,
    /// The realname sent in `USER`. Defaults to the nick.
    #[cfg_attr(feature = "ts", ts(as = "Option<String>", optional))]
    pub realname: String,
    /// The server password, sent as `PASS` before anything else.
    #[cfg_attr(feature = "ts", ts(optional))]
    pub password: Option<String>,
    /// What to authenticate with, when the server offers `sasl`.
    #[cfg_attr(feature = "ts", ts(optional))]
    pub sasl: Option<Credentials>,
    /// How many messages each channel and conversation keeps.
    #[cfg_attr(feature = "ts", ts(as = "Option<usize>", optional))]
    pub retention: usize,
    /// Nicks to fall back through when the server says ours is taken during registration.
    ///
    /// Registration stalls forever if nobody answers a 433, so the engine walks this list and then
    /// starts appending underscores rather than leaving the connection hung.
    #[cfg_attr(feature = "ts", ts(as = "Option<Vec<String>>", optional))]
    pub alt_nicks: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self::new(String::new())
    }
}

impl Config {
    /// A config that registers with one nick and nothing else.
    pub fn new(nick: impl Into<String>) -> Self {
        let nick = nick.into();
        Self {
            username: nick.clone(),
            realname: nick.clone(),
            nick,
            password: None,
            sasl: None,
            retention: crate::model::DEFAULT_RETENTION,
            alt_nicks: Vec::new(),
        }
    }
}

/// Something the host needs to know about.
///
/// Events are drained with [`Client::poll_event`] rather than delivered through a callback, because
/// a callback needs a different lifetime, threading and re-entrancy contract in every language this
/// engine is bound into.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "ObbyEvent"))]
#[cfg_attr(feature = "serde", serde(tag = "type", rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Event {
    /// A capability was acknowledged, and whatever it enables is now live.
    CapAcknowledged {
        /// The capability names, as acknowledged.
        names: Vec<String>,
    },
    /// Registration finished. `001` arrived and the nick is settled.
    Registered {
        /// The nick the server actually gave us, which may differ from the one we asked for.
        nick: String,
    },
    /// An ISUPPORT token arrived. Emitted per token so a host can react to one without diffing.
    Isupport {
        /// The token name, such as `CHANMODES`.
        token: String,
        /// Its value, absent for a boolean token, present and empty for `TOKEN=`.
        value: Option<String>,
    },
    /// Authentication succeeded.
    LoggedIn {
        /// The account the server logged us in as.
        account: String,
    },
    /// Authentication did not succeed. Registration continues unauthenticated.
    SaslFailed {
        /// Why it ended.
        reason: SaslFailure,
    },
    /// The server refused a nick and the engine is trying another.
    NickInUse {
        /// The nick that was refused.
        refused: String,
        /// What is being tried instead.
        trying: String,
    },
    /// The model changed.
    Changed {
        /// What changed.
        change: Change,
    },
    /// The link went quiet for too long and should be treated as dead. The host closes its socket
    /// and waits for [`Event::Reconnect`].
    LinkDead,
    /// Time to open a new connection. The host dials and calls [`Client::connected`].
    Reconnect {
        /// How long the host should wait first.
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        after_ms: u64,
    },
    /// Reconnecting has been given up on after too many attempts.
    ReconnectGaveUp,
    /// A command we labelled never got its reply.
    CommandTimedOut {
        /// The command that went unanswered.
        command: String,
    },
    /// The set of commands the server allows us changed.
    #[cfg(feature = "obby")]
    CommandsChanged,
    /// A voice signalling frame arrived, and the room state has already been updated for it.
    ///
    /// The host handles the media plane: this carries the session descriptions and candidates it
    /// needs, and nothing that touches a track.
    #[cfg(feature = "voice")]
    Voice {
        /// The channel the room belongs to.
        channel: String,
        /// The frame.
        signal: crate::voice::Signal,
    },
    /// Someone started or stopped composing a message.
    Typing {
        /// The channel or conversation they are composing in.
        target: String,
        /// Who.
        nick: String,
        /// True when they are composing now.
        typing: bool,
    },
    /// Someone we watch came online or went offline.
    Presence {
        /// Who.
        nick: String,
        /// True when they are here now.
        online: bool,
    },
    /// The server reported the outcome of something, through `standard-replies`.
    Reply {
        /// How serious it is.
        severity: Severity,
        /// The command it concerns, or `*` when the server did not say.
        command: String,
        /// The machine-readable code, such as `ACCOUNT_REQUIRED_TO_CONNECT`.
        code: String,
        /// Any further parameters the server gave, before the description.
        context: Vec<String>,
        /// The human-readable description.
        text: String,
    },
    /// A line arrived that the engine has no dedicated handling for yet. Everything is surfaced, so
    /// a host is never blind to traffic the engine does not model.
    Raw {
        /// The parsed line.
        message: Message,
    },
}

/// One connection.
#[derive(Debug)]
pub struct Client {
    config: Config,
    phase: Phase,
    caps: Caps,
    nick: String,
    isupport: Isupport,
    model: Model,
    batches: Batches,
    monitor: Monitor,
    /// Channel keys, so a rejoin after a reconnect is not refused for want of one.
    channel_keys: alloc::collections::BTreeMap<obby_proto::CaseFolded, String>,
    /// Metadata keys we subscribed to, so a reconnect can ask again.
    metadata_subscriptions: Vec<String>,
    #[cfg(feature = "voice")]
    rooms: alloc::collections::BTreeMap<obby_proto::CaseFolded, crate::voice::Room>,
    #[cfg(feature = "obby")]
    commands: crate::extensions::Commands,
    timers: Timers,
    labels: Labels<String>,
    backoff: ReconnectBackoff,
    monotonic_ms: u64,
    /// The latest instant seen, which stamps a line the server did not stamp itself.
    latest_ms: u64,
    /// True while we are skipping the remainder of a line that grew past what we will hold.
    discarding: bool,
    dropped_lines: u64,
    sasl: SaslState,
    scram: Option<crate::scram::Scram>,
    nick_attempt: usize,
    /// Bytes read from the transport that do not yet form a whole line.
    partial: Vec<u8>,
    outbox: VecDeque<Vec<u8>>,
    events: VecDeque<Event>,
}

impl Client {
    /// Build an engine that has not connected yet. Nothing is written until [`Client::connected`].
    pub fn new(config: Config) -> Self {
        let mut config = config;
        // a host that deserialised a partial config gets these filled in rather than registering
        // with an empty username, which some servers refuse outright
        if config.username.is_empty() {
            config.username.clone_from(&config.nick);
        }
        if config.realname.is_empty() {
            config.realname.clone_from(&config.nick);
        }
        let mut model = Model::with_retention(config.retention);
        model.me.nick.clone_from(&config.nick);
        Self {
            nick: config.nick.clone(),
            model,
            latest_ms: 0,
            discarding: false,
            dropped_lines: 0,
            config,
            phase: Phase::Disconnected,
            caps: Caps::default(),
            isupport: Isupport::default(),
            batches: Batches::new(),
            monitor: Monitor::new(),
            channel_keys: alloc::collections::BTreeMap::new(),
            metadata_subscriptions: Vec::new(),
            #[cfg(feature = "voice")]
            rooms: alloc::collections::BTreeMap::new(),
            #[cfg(feature = "obby")]
            commands: crate::extensions::Commands::new(),
            timers: Timers::new(),
            labels: Labels::new(),
            backoff: ReconnectBackoff::default(),
            monotonic_ms: 0,
            sasl: SaslState::default(),
            scram: None,
            nick_attempt: 0,
            partial: Vec::new(),
            outbox: VecDeque::new(),
            events: VecDeque::new(),
        }
    }

    /// Tell the engine the transport is up. This queues the registration burst.
    ///
    /// The host calls this once the socket is open and the TLS handshake, if any, has finished. The
    /// engine has no way to know that on its own.
    pub fn connected(&mut self) {
        self.phase = Phase::Negotiating;
        self.backoff.reset();
        self.timers.clear(&Deadline::Reconnect);
        self.arm_keepalive();
        // CAP LS goes first so the server holds registration open while we negotiate. PASS has to
        // precede NICK and USER, and NICK and USER may be sent during negotiation.
        self.send(&Message::new("CAP", ["LS", "302"]));
        if let Some(password) = self.config.password.clone() {
            self.send(&Message::new("PASS", [password]));
        }
        self.send(&Message::new("NICK", [self.config.nick.clone()]));
        self.send(&Message::new(
            "USER",
            [
                self.config.username.clone(),
                "0".to_string(),
                "*".to_string(),
                self.config.realname.clone(),
            ],
        ));
    }

    /// Tell the engine its transport died. The model survives, so a reconnect can resume from it.
    ///
    /// The engine decides when to try again and says so with [`Event::Reconnect`]. The host owns
    /// every socket, and this is the only thing it has to report.
    pub fn disconnected(&mut self) {
        self.phase = Phase::Disconnected;
        self.batches.drop_all();
        self.caps = Caps::default();
        self.sasl = SaslState::default();
        self.monitor.forget_presence();
        // who was in a channel is only true while we are connected to hear about it. The messages
        // stay, because they happened; the member lists do not, because they are a live view
        for (_, channel) in self.model.channels_mut() {
            channel.members.clear();
        }
        self.timers.clear(&Deadline::PingKeepalive);
        self.timers.clear(&Deadline::DeadLink);
        match self.backoff.next_delay_ms() {
            Some(after_ms) => {
                self.timers.set(
                    Deadline::Reconnect,
                    self.monotonic_ms.saturating_add(after_ms),
                );
                self.events.push_back(Event::Reconnect { after_ms });
            }
            None => self.events.push_back(Event::ReconnectGaveUp),
        }
    }

    /// Advance the clock. Whatever has fallen due happens here.
    ///
    /// The host supplies both clocks because the engine has neither: the monotonic one drives every
    /// deadline, and the wall clock only stamps a message the server did not stamp itself.
    pub fn tick(&mut self, now: Now) {
        self.monotonic_ms = now.monotonic_ms;
        self.latest_ms = self.latest_ms.max(now.unix_ms);

        for deadline in self.timers.expire(now) {
            match deadline {
                Deadline::PingKeepalive => {
                    self.send(&Message::new("PING", [self.nick.clone()]));
                    self.timers.set(
                        Deadline::DeadLink,
                        now.monotonic_ms.saturating_add(DEAD_LINK_MS),
                    );
                }
                Deadline::DeadLink => self.events.push_back(Event::LinkDead),
                Deadline::Reconnect => self.events.push_back(Event::Reconnect { after_ms: 0 }),
                Deadline::Typing(target, who) => self.expire_typing(&target, &who),
            }
        }

        for command in self.labels.expire(now.monotonic_ms) {
            self.events.push_back(Event::CommandTimedOut { command });
        }
    }

    /// When [`Client::tick`] next has something to do, as a monotonic instant.
    ///
    /// A host can sleep until exactly then rather than waking on a fixed interval.
    pub fn poll_timeout(&self) -> Option<u64> {
        self.timers.next()
    }

    /// Do something on this connection.
    ///
    /// Anything the server will echo back to us is left for that echo to record, so a message never
    /// lands twice. Without `echo-message` there is no echo coming, so we record it ourselves from
    /// the same line we sent, through the same path that would have handled the echo.
    pub fn command(&mut self, command: Command) {
        // the three that land in a conversation need the model, the rest are pure translation
        match command {
            Command::Message { target, text } => self.say(&Message::new("PRIVMSG", [target, text])),
            Command::Notice { target, text } => self.say(&Message::new("NOTICE", [target, text])),
            Command::Action { target, text } => {
                let body = alloc::format!("\u{1}ACTION {text}\u{1}");
                self.say(&Message::new("PRIVMSG", [target, body]));
            }
            // both are remembered, because a reconnect has to reproduce them and the server will
            // not tell us what we asked for last time
            Command::Join { channel, key } => {
                let folded = self.isupport.fold(&channel);
                match &key {
                    Some(key) => self.channel_keys.insert(folded, key.clone()),
                    None => self.channel_keys.remove(&folded),
                };
                let line = match key {
                    Some(key) => Message::new("JOIN", [channel, key]),
                    None => Message::new("JOIN", [channel]),
                };
                self.send_labeled(&line);
            }
            Command::SubscribeMetadata { keys } => {
                for key in &keys {
                    if !self.metadata_subscriptions.contains(key) {
                        self.metadata_subscriptions.push(key.clone());
                    }
                }
                let mut params = alloc::vec!["*".to_string(), "SUB".to_string()];
                params.extend(keys);
                self.send_labeled(&Message::new("METADATA", params));
            }
            Command::Watch { nicks } => self.set_watching(&nicks, true),
            Command::Unwatch { nicks } => self.set_watching(&nicks, false),
            other => {
                if let Some(line) = Self::wire(other) {
                    self.send_labeled(&line);
                }
            }
        }
    }

    /// Turn a command into the line that carries it.
    ///
    /// `None` for a raw line that will not parse, which must not reach the wire half-formed.
    fn wire(command: Command) -> Option<Message> {
        Some(match command {
            Command::Message { .. }
            | Command::Notice { .. }
            | Command::Action { .. }
            | Command::Watch { .. }
            | Command::Unwatch { .. } => return None,
            Command::Join { .. } | Command::SubscribeMetadata { .. } => return None,
            Command::Part { channel, reason } => match reason {
                Some(reason) => Message::new("PART", [channel, reason]),
                None => Message::new("PART", [channel]),
            },
            Command::Nick { nick } => Message::new("NICK", [nick]),
            Command::Topic { channel, topic } => match topic {
                Some(topic) => Message::new("TOPIC", [channel, topic]),
                // an empty trailing parameter clears a topic; omitting it asks what the topic is
                None => Message::new("TOPIC", [channel, String::new()]),
            },
            Command::Away { message } => match message {
                Some(message) => Message::new("AWAY", [message]),
                None => Message::new("AWAY", [] as [String; 0]),
            },
            Command::Typing { target, state } => {
                let mut line = Message::new("TAGMSG", [target]);
                line.tags
                    .set(obby_proto::Tag::new("+typing", state.as_str()));
                line
            }
            Command::React {
                target,
                msgid,
                emoji,
            } => Self::reaction("+draft/react", &target, &msgid, &emoji),
            Command::Unreact {
                target,
                msgid,
                emoji,
            } => Self::reaction("+draft/unreact", &target, &msgid, &emoji),
            Command::Redact {
                target,
                msgid,
                reason,
            } => match reason {
                Some(reason) => Message::new("REDACT", [target, msgid, reason]),
                None => Message::new("REDACT", [target, msgid]),
            },
            Command::MarkRead { target, timestamp } => Message::new(
                "MARKREAD",
                [target, alloc::format!("timestamp={timestamp}")],
            ),
            Command::History {
                target,
                before,
                limit,
            } => match before {
                Some(msgid) => Message::new(
                    "CHATHISTORY",
                    [
                        "BEFORE".to_string(),
                        target,
                        alloc::format!("msgid={msgid}"),
                        limit.to_string(),
                    ],
                ),
                None => Message::new(
                    "CHATHISTORY",
                    [
                        "LATEST".to_string(),
                        target,
                        "*".to_string(),
                        limit.to_string(),
                    ],
                ),
            },
            Command::SetMetadata { key, value } => match value {
                Some(value) => {
                    Message::new("METADATA", ["*".to_string(), "SET".to_string(), key, value])
                }
                // a SET with no value is how the specification spells a delete
                None => Message::new("METADATA", ["*".to_string(), "SET".to_string(), key]),
            },
            #[cfg(feature = "voice")]
            Command::Voice { channel, payload } => {
                let mut line = Message::new("TAGMSG", [channel]);
                line.tags
                    .set(obby_proto::Tag::new("+obsidianirc/rtc", payload));
                line
            }
            Command::Quit { reason } => match reason {
                Some(reason) => Message::new("QUIT", [reason]),
                None => Message::new("QUIT", [] as [String; 0]),
            },
            Command::Raw { line } => Message::parse(&line).ok()?,
        })
    }

    /// Start or stop watching a set of nicks, in requests the server will accept whole.
    fn set_watching(&mut self, nicks: &[String], watching: bool) {
        let folded: Vec<obby_proto::CaseFolded> =
            nicks.iter().map(|nick| self.isupport.fold(nick)).collect();
        if watching {
            self.monitor.watch(folded);
        } else {
            self.monitor.unwatch(&folded);
        }
        let limit = self.isupport.number("MONITOR").unwrap_or(100) as usize;
        let verb = if watching { "+" } else { "-" };
        for chunk in monitor::batched(nicks, limit) {
            self.send(&Message::new("MONITOR", [verb.to_string(), chunk]));
        }
    }

    fn reaction(tag: &str, target: &str, msgid: &str, emoji: &str) -> Message {
        let mut line = Message::new("TAGMSG", [target]);
        line.tags.set(obby_proto::Tag::new(tag, emoji));
        line.tags.set(obby_proto::Tag::new("+draft/reply", msgid));
        line
    }

    /// Send something that lands in a conversation, recording it ourselves if no echo is coming.
    fn say(&mut self, line: &Message) {
        self.send_labeled(line);
        if self.caps.has("echo-message") {
            return;
        }
        let mut echo = line.clone();
        echo.source = Some(obby_proto::Source {
            name: self.nick.clone(),
            user: None,
            host: None,
        });
        self.fold(&echo, false);
    }

    /// Send a command and correlate whatever the server sends back with it.
    ///
    /// Returns the label when `labeled-response` is in force. Without that capability the command
    /// still goes out, unlabelled, because a server that does not support it would only be confused
    /// by the tag.
    pub fn send_labeled(&mut self, message: &Message) -> Option<String> {
        if !self.caps.has("labeled-response") {
            self.send(message);
            return None;
        }
        let label = self.labels.generate();
        let mut message = message.clone();
        message
            .tags
            .set(obby_proto::Tag::new("label", label.clone()));
        self.labels.register(
            label.clone(),
            self.monotonic_ms.saturating_add(DEFAULT_TIMEOUT_MS),
            message.command.clone(),
        );
        self.send(&message);
        Some(label)
    }

    /// Put the connection back where it was.
    ///
    /// The model outlived the link, so the channels we were in are still here and we rejoin every
    /// one. On a first connection there are none and this does nothing.
    fn resume(&mut self) {
        let rejoining: Vec<(String, Option<String>)> = self
            .model
            .channels()
            .map(|(_, channel)| {
                (
                    channel.name.clone(),
                    channel.log.last().and_then(|message| message.msgid.clone()),
                )
            })
            .collect();

        let watched: Vec<String> = self
            .monitor
            .watched()
            .map(|folded| folded.as_str().to_string())
            .collect();
        if !watched.is_empty() {
            let limit = self.isupport.number("MONITOR").unwrap_or(100) as usize;
            for chunk in monitor::batched(&watched, limit) {
                self.send(&Message::new("MONITOR", ["+".to_string(), chunk]));
            }
        }

        if !self.metadata_subscriptions.is_empty() {
            let mut params = alloc::vec!["*".to_string(), "SUB".to_string()];
            params.extend(self.metadata_subscriptions.clone());
            self.send(&Message::new("METADATA", params));
        }

        for (name, newest) in rejoining {
            let key = self.channel_keys.get(&self.isupport.fold(&name)).cloned();
            let join = match key {
                Some(key) => Message::new("JOIN", [name.clone(), key]),
                None => Message::new("JOIN", [name.clone()]),
            };
            self.send(&join);
            // ask only for what happened while we were gone, rather than refetching a whole window
            // and leaning on dedup to sort it out
            if let (true, Some(msgid)) = (self.caps.has("draft/chathistory"), newest) {
                self.send(&Message::new(
                    "CHATHISTORY",
                    [
                        "AFTER".to_string(),
                        name,
                        alloc::format!("msgid={msgid}"),
                        "100".to_string(),
                    ],
                ));
            }
        }
    }

    fn arm_keepalive(&mut self) {
        self.timers.clear(&Deadline::DeadLink);
        self.timers.set(
            Deadline::PingKeepalive,
            self.monotonic_ms.saturating_add(PING_KEEPALIVE_MS),
        );
    }

    /// Feed whatever the transport read. Partial lines are held until the rest arrives.
    pub fn handle_bytes(&mut self, data: &[u8]) {
        self.partial.extend_from_slice(data);
        loop {
            let Some(end) = self.partial.iter().position(|b| *b == b'\n') else {
                if self.partial.len() > MAX_INBOUND_LINE {
                    self.partial.clear();
                    self.discarding = true;
                    self.dropped_lines = self.dropped_lines.saturating_add(1);
                }
                return;
            };
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            if self.discarding {
                self.discarding = false;
                continue;
            }
            // servers are not reliably UTF-8, and one bad byte must not cost us the line
            let text = String::from_utf8_lossy(&line);
            if let Ok(message) = Message::parse(&text) {
                self.handle_message(message);
            } else {
                self.dropped_lines = self.dropped_lines.saturating_add(1);
            }
        }
    }

    /// How many inbound lines were dropped, either unparseable or longer than we will hold.
    ///
    /// Nothing is returned to the host when a line is dropped, because there is nothing it could do
    /// about it. The count is here so a host can see that it is happening at all.
    pub fn dropped_lines(&self) -> u64 {
        self.dropped_lines
    }

    /// Bytes the host should write to the transport, or `None` when there are none.
    pub fn poll_transmit(&mut self) -> Option<Vec<u8>> {
        self.outbox.pop_front()
    }

    /// The next thing that happened, or `None` when the host is caught up.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// Queue a message to be written. Use this for anything the engine does not model yet.
    pub fn send(&mut self, message: &Message) {
        let mut line = alloc::format!("{message}").into_bytes();
        line.extend_from_slice(b"\r\n");
        self.outbox.push_back(line);
    }

    /// How far registration has got.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The nick the server currently knows us by.
    pub fn nick(&self) -> &str {
        &self.nick
    }

    /// The capabilities the server offers and the ones we hold.
    pub fn caps(&self) -> &Caps {
        &self.caps
    }

    /// Everything the server advertised about itself: casemapping, prefixes, mode classes, limits.
    pub fn isupport(&self) -> &Isupport {
        &self.isupport
    }

    /// The commands the server says we may currently use.
    ///
    /// Empty until the server sends its list, which it does on connect and again whenever what we
    /// may do changes, such as after an `OPER`.
    #[cfg(feature = "obby")]
    pub fn allowed_commands(&self) -> &crate::extensions::Commands {
        &self.commands
    }

    /// The voice room for a channel, once we have heard any signalling for it.
    ///
    /// Signalling and room state only. Every track, codec and peer connection is the host's, and
    /// nothing here knows they exist.
    #[cfg(feature = "voice")]
    pub fn room(&self, channel: &obby_proto::CaseFolded) -> Option<&crate::voice::Room> {
        self.rooms.get(channel)
    }

    /// Who we are watching for coming online, and who is here.
    pub fn monitor(&self) -> &Monitor {
        &self.monitor
    }

    /// Everything the connection knows: channels, members, conversations, messages.
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// The casemapping to fold nicks and channel names with, from ISUPPORT.
    pub fn casemapping(&self) -> Casemapping {
        self.isupport.casemapping()
    }

    fn handle_message(&mut self, message: Message) {
        // any line at all proves the link is alive, so the keepalive clock starts over
        self.arm_keepalive();
        self.labels.resolve(&message);
        if message.is("PONG") {
            return;
        }
        if message.is("PING") {
            let token = message.trailing().unwrap_or_default().to_string();
            self.send(&Message::new("PONG", [token]));
            return;
        }
        if message.is("CAP") {
            self.handle_cap(&message);
            return;
        }
        if message.is("001") {
            self.phase = Phase::Registered;
            if let Some(nick) = message.param(0) {
                self.nick = nick.to_string();
            }
            self.model.me.nick.clone_from(&self.nick);
            self.events.push_back(Event::Registered {
                nick: self.nick.clone(),
            });
            self.resume();
            return;
        }
        if message.is("005") {
            self.handle_isupport(&message);
            return;
        }
        if message.is("TAGMSG") && self.handle_typing(&message) {
            return;
        }
        #[cfg(feature = "voice")]
        if message.is("TAGMSG") && self.handle_rtc(&message) {
            return;
        }
        #[cfg(feature = "obby")]
        if message.is("CMDSLIST") {
            self.commands.apply(&message);
            self.events.push_back(Event::CommandsChanged);
            return;
        }
        if matches!(message.command.as_str(), "730" | "731") {
            self.handle_monitor(&message);
            return;
        }
        if let Some(severity) = severity_of(&message.command)
            && self.handle_standard_reply(severity, &message)
        {
            return;
        }
        if message.is("AUTHENTICATE") {
            self.handle_authenticate(&message);
            return;
        }
        if message.is("433") || message.is("432") {
            self.handle_nick_refused(&message);
            return;
        }
        if let Some(reason) = sasl_failure(&message.command) {
            self.finish_sasl(Some(reason));
            return;
        }
        // 900 carries the account name; 903 only confirms. A server sends both, so the account is
        // remembered from 900 and reported when 903 settles the exchange.
        if message.is("900") {
            if let Some(account) = message.param(2) {
                self.events.push_back(Event::LoggedIn {
                    account: account.to_string(),
                });
            }
            return;
        }
        if message.is("903") {
            self.finish_sasl(None);
            return;
        }
        if message.is("BATCH") {
            self.handle_batch(&message);
            return;
        }
        // a line inside an open batch is held until the batch closes, so history arrives as one
        // consistent block rather than trickling in and reordering what is already shown
        let Some(message) = self.batches.route(message) else {
            return;
        };
        self.fold(&message, false);
    }

    fn handle_batch(&mut self, message: &Message) {
        let reference = message.param(0).unwrap_or_default();
        if reference.starts_with('+') {
            self.batches.open(message);
            return;
        }
        let Some(closed) = self.batches.close(message) else {
            return;
        };
        self.drain_batch(closed);
    }

    fn drain_batch(&mut self, closed: ClosedBatch) {
        // an inner batch closes while its parent is still open, so the ancestor chain is what says
        // whether these lines are replayed history
        let historical = closed.kind == CHATHISTORY_BATCH
            || closed
                .parent
                .as_deref()
                .is_some_and(|parent| self.batches.is_within(parent, CHATHISTORY_BATCH));
        for message in closed.messages {
            self.fold(&message, historical);
        }
    }

    /// Fold one line into the model and surface whatever it changed.
    fn fold(&mut self, message: &Message, historical: bool) {
        let changes = {
            let mut ctx = session::Context {
                model: &mut self.model,
                isupport: &self.isupport,
                latest_ms: &mut self.latest_ms,
                historical,
            };
            session::apply(&mut ctx, message)
        };
        if changes.is_empty() {
            self.events.push_back(Event::Raw {
                message: message.clone(),
            });
            return;
        }
        for change in &changes {
            // NAMES gives us nicks and prefixes and nothing else, so joining is when we go and ask
            // who these people actually are
            if let Change::Joined { channel } = change {
                self.request_who(channel);
            }
        }
        for change in changes {
            self.events.push_back(Event::Changed { change });
        }
    }

    /// Ask who is in a channel, in the richest form the server supports.
    ///
    /// WHOX carries the account, which a plain WHO does not, and the account is what tells a client
    /// that two nicks are the same person.
    fn request_who(&mut self, channel: &str) {
        if self.isupport.has("WHOX") {
            self.send(&Message::new(
                "WHO",
                [
                    channel.to_string(),
                    alloc::format!("{},{}", session::WHOX_FIELDS, session::WHOX_TOKEN),
                ],
            ));
        } else {
            self.send(&Message::new("WHO", [channel]));
        }
    }

    fn handle_cap(&mut self, message: &Message) {
        // CAP replies are `CAP <target> <subcommand> [*] :<list>`, where the `*` marks a
        // continuation and the list is the last parameter either way.
        let Some(subcommand) = message.param(1) else {
            return;
        };
        let list = message.trailing().unwrap_or_default();
        let more_to_come = message.param(2) == Some("*");

        match subcommand.to_ascii_uppercase().as_str() {
            "LS" => {
                self.caps.advertise(list);
                if !more_to_come {
                    self.request_wanted();
                }
            }
            "NEW" => {
                self.caps.advertise(list);
                self.request_wanted();
            }
            "DEL" => self.caps.withdraw(list),
            "ACK" => {
                let names = self.caps.acknowledge(list);
                if !names.is_empty() {
                    self.events.push_back(Event::CapAcknowledged { names });
                }
                self.finish_negotiation_if_settled();
            }
            "NAK" => {
                self.caps.reject(list);
                self.finish_negotiation_if_settled();
            }
            _ => {}
        }
    }

    fn request_wanted(&mut self) {
        let wanted = self.caps.to_request();
        if wanted.is_empty() {
            self.finish_negotiation_if_settled();
            return;
        }
        self.caps.requested(&wanted);
        // one REQ per line, and the server answers each atomically: all of a REQ is acked or none of
        // it is, so a rejected capability never takes the rest of the line down with it
        for name in wanted {
            self.send(&Message::new("CAP", ["REQ".to_string(), name]));
        }
    }

    fn finish_negotiation_if_settled(&mut self) {
        if self.phase != Phase::Negotiating || !self.caps.settled() {
            return;
        }
        if self.start_sasl_if_wanted() || self.sasl.in_flight() {
            return;
        }
        self.phase = Phase::Registering;
        self.send(&Message::new("CAP", ["END"]));
    }

    /// Open the exchange, if there is one to open. True when we are now waiting on the server.
    fn start_sasl_if_wanted(&mut self) -> bool {
        if self.sasl != SaslState::Idle || !self.caps.has("sasl") {
            return false;
        }
        let Some(credentials) = self.config.sasl.clone() else {
            return false;
        };
        let mechanism = credentials.mechanism();
        if !sasl::offers(self.caps.value("sasl"), mechanism) {
            self.sasl = SaslState::Settled;
            self.events.push_back(Event::SaslFailed {
                reason: SaslFailure::NoSharedMechanism,
            });
            return false;
        }
        self.sasl = SaslState::Offered;
        self.send(&Message::new("AUTHENTICATE", [mechanism]));
        true
    }

    fn handle_authenticate(&mut self, message: &Message) {
        let Some(challenge) = sasl::decode_challenge(message.param(0).unwrap_or("+")) else {
            self.finish_sasl(Some(SaslFailure::Aborted));
            return;
        };
        match self.sasl {
            SaslState::Offered => self.begin_sasl_response(),
            SaslState::ScramChallenged => self.answer_scram_challenge(&challenge),
            SaslState::ScramProved => self.verify_scram_server(&challenge),
            _ => {}
        }
    }

    /// Answer the server's invitation with whatever the mechanism opens with.
    fn begin_sasl_response(&mut self) {
        let Some(credentials) = self.config.sasl.clone() else {
            return;
        };
        if let Credentials::Scram {
            username,
            password,
            nonce,
        } = &credentials
        {
            let scram = crate::scram::Scram::new(username, password, nonce);
            let first = scram.client_first();
            self.scram = Some(scram);
            self.sasl = SaslState::ScramChallenged;
            self.send_sasl(&first);
            return;
        }
        self.sasl = SaslState::Responded;
        self.send_sasl(&credentials.response());
    }

    /// Prove we know the password, over the salt and nonce the server just sent.
    fn answer_scram_challenge(&mut self, challenge: &[u8]) {
        let Some(scram) = self.scram.as_mut() else {
            self.finish_sasl(Some(SaslFailure::Aborted));
            return;
        };
        match scram.client_final(challenge) {
            Ok(response) => {
                self.sasl = SaslState::ScramProved;
                self.send_sasl(&response);
            }
            Err(_) => self.finish_sasl(Some(SaslFailure::Rejected)),
        }
    }

    /// Check that the server knows the password too.
    ///
    /// Skipping this is how a server that never knew it convinces us that it did, so a failure here
    /// aborts rather than continuing.
    fn verify_scram_server(&mut self, challenge: &[u8]) {
        let Some(scram) = self.scram.as_ref() else {
            self.finish_sasl(Some(SaslFailure::Aborted));
            return;
        };
        if scram.verify(challenge).is_err() {
            self.scram = None;
            self.finish_sasl(Some(SaslFailure::ServerNotVerified));
            return;
        }
        self.scram = None;
        self.sasl = SaslState::Responded;
        // an empty response closes the exchange and lets the server send its verdict
        self.send(&Message::new("AUTHENTICATE", ["+"]));
    }

    fn send_sasl(&mut self, payload: &[u8]) {
        for line in sasl::encode_response(payload) {
            self.send(&Message::new("AUTHENTICATE", [line]));
        }
    }

    /// End the exchange either way and release the `CAP END` that was waiting on it.
    fn finish_sasl(&mut self, failure: Option<SaslFailure>) {
        if self.sasl == SaslState::Settled {
            return;
        }
        self.sasl = SaslState::Settled;
        if let Some(reason) = failure {
            self.events.push_back(Event::SaslFailed { reason });
        }
        self.finish_negotiation_if_settled();
    }

    /// Walk the fallback nicks, then append underscores. Only during registration: after `001` the
    /// host asked for the rename and gets to decide what to do about it.
    fn handle_nick_refused(&mut self, message: &Message) {
        if self.phase == Phase::Registered {
            self.events.push_back(Event::Raw {
                message: message.clone(),
            });
            return;
        }
        let refused = message.param(1).unwrap_or(&self.nick).to_string();
        let trying = match self.config.alt_nicks.get(self.nick_attempt) {
            Some(alt) => alt.clone(),
            None => alloc::format!("{}_", self.nick),
        };
        self.nick_attempt += 1;
        self.nick.clone_from(&trying);
        self.model.me.nick.clone_from(&trying);
        self.events.push_back(Event::NickInUse {
            refused,
            trying: trying.clone(),
        });
        self.send(&Message::new("NICK", [trying]));
    }

    /// Apply a voice signalling frame, if the line carries one.
    ///
    /// Signalling rides on a TAGMSG to the channel and is never a message, so it is consumed here
    /// rather than falling through to the conversation.
    #[cfg(feature = "voice")]
    fn handle_rtc(&mut self, message: &Message) -> bool {
        let Some(payload) = message.tag("+obsidianirc/rtc") else {
            return false;
        };
        let Some(channel) = message.param(0) else {
            return false;
        };
        let Some(signal) = crate::voice::Signal::from_json(payload) else {
            // a frame we cannot read is still signalling, and filing it as a message would put an
            // empty row in the channel
            self.dropped_lines = self.dropped_lines.saturating_add(1);
            return true;
        };
        let folded = self.isupport.fold(channel);
        let room = self
            .rooms
            .entry(folded)
            .or_insert_with(|| crate::voice::Room::new(channel));
        room.apply(&self.nick, &signal, self.isupport.casemapping());
        self.events.push_back(Event::Voice {
            channel: channel.to_string(),
            signal,
        });
        true
    }

    /// Apply a `+typing` tag, if the line carries one.
    ///
    /// A typing indicator is not a message and must not appear as one. It also expires on its own,
    /// because the `done` that would clear it can be lost, and a client that waits for it shows
    /// someone typing forever.
    fn handle_typing(&mut self, message: &Message) -> bool {
        let Some(state) = message.tag("+typing") else {
            return false;
        };
        let (Some(target), Some(source)) = (message.param(0), message.source.as_ref()) else {
            return false;
        };
        let who = source.name.clone();
        // a message to us belongs to the conversation with the sender, not to one named after us
        let target = if self.isupport.is_channel(target) {
            target.to_string()
        } else {
            who.clone()
        };
        let folded_target = self.isupport.fold(&target);
        let folded_who = self.isupport.fold(&who);
        let typing = state == "active";

        let deadline = Deadline::Typing(
            folded_target.as_str().to_string(),
            folded_who.as_str().to_string(),
        );
        if typing {
            self.timers
                .set(deadline, self.monotonic_ms.saturating_add(TYPING_EXPIRY_MS));
        } else {
            self.timers.clear(&deadline);
        }

        if self.model.set_typing(&folded_target, folded_who, typing) {
            self.events.push_back(Event::Typing {
                target,
                nick: who,
                typing,
            });
        }
        true
    }

    /// Stop showing someone as typing once their indicator has gone stale.
    fn expire_typing(&mut self, target: &str, who: &str) {
        let (target, who) = (
            obby_proto::CaseFolded::already_folded(target),
            obby_proto::CaseFolded::already_folded(who),
        );
        if self.model.set_typing(&target, who.clone(), false) {
            self.events.push_back(Event::Typing {
                target: target.into_string(),
                nick: who.into_string(),
                typing: false,
            });
        }
    }

    /// Apply `730` (online) and `731` (offline), each carrying a comma-separated target list.
    fn handle_monitor(&mut self, message: &Message) {
        let online = message.is("730");
        let Some(list) = message.param(1) else { return };
        for target in monitor::split_targets(list) {
            // the list carries full masks, and only the nick identifies the person
            let nick = target.split('!').next().unwrap_or(&target).to_string();
            let folded = self.isupport.fold(&nick);
            if online {
                self.monitor.mark_online(folded);
            } else {
                self.monitor.mark_offline(&folded);
            }
            self.events.push_back(Event::Presence { nick, online });
        }
    }

    /// Surface a `FAIL`, `WARN` or `NOTE`.
    ///
    /// The shape is `<command> <code> [context...] :<description>`, where the description is always
    /// last and everything between the code and it is context whose meaning depends on the code.
    /// Returns false when the line does not have the shape, so the caller still surfaces it raw.
    fn handle_standard_reply(&mut self, severity: Severity, message: &Message) -> bool {
        let command = message.param(0).unwrap_or("*").to_string();
        let Some(code) = message.param(1).map(ToString::to_string) else {
            return false;
        };
        let text = message.trailing().unwrap_or_default().to_string();
        let context: Vec<String> = message
            .params
            .iter()
            .skip(2)
            .take(message.params.len().saturating_sub(3))
            .cloned()
            .collect();
        self.events.push_back(Event::Reply {
            severity,
            command,
            code,
            context,
            text,
        });
        true
    }

    fn handle_isupport(&mut self, message: &Message) {
        // params are `<nick> <token>... :are supported by this server`, so the first and last are
        // not tokens
        let tokens = message
            .params
            .iter()
            .skip(1)
            .take(message.params.len().saturating_sub(2));
        for token in tokens {
            let applied = self.isupport.apply(token);
            self.events.push_back(Event::Isupport {
                token: applied.name,
                value: applied.value,
            });
        }
    }
}

/// How serious a `standard-replies` message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Severity {
    /// The command did not happen.
    Fail,
    /// The command happened, with a caveat.
    Warn,
    /// Something worth saying, with nothing gone wrong.
    Note,
}

/// The most we will buffer while assembling one inbound line.
///
/// A stream that never sends a newline, whether hostile or merely stuck, would otherwise grow the
/// buffer until the process dies. This is the largest line any server may legally send: the tag
/// budget plus the rest of the line.
const MAX_INBOUND_LINE: usize = obby_proto::MAX_TAG_BYTES + obby_proto::MAX_LINE_BYTES;

/// The batch type that carries replayed history.
const CHATHISTORY_BATCH: &str = "chathistory";

/// Read the severity off a `standard-replies` verb.
fn severity_of(command: &str) -> Option<Severity> {
    match command.to_ascii_uppercase().as_str() {
        "FAIL" => Some(Severity::Fail),
        "WARN" => Some(Severity::Warn),
        "NOTE" => Some(Severity::Note),
        _ => None,
    }
}

/// Map a SASL error numeric onto why it failed.
fn sasl_failure(command: &str) -> Option<SaslFailure> {
    match command {
        "904" => Some(SaslFailure::Rejected),
        "905" => Some(SaslFailure::TooLong),
        "906" => Some(SaslFailure::Aborted),
        "907" => Some(SaslFailure::AlreadyAuthenticated),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(client: &mut Client) -> String {
        let mut out = String::new();
        while let Some(bytes) = client.poll_transmit() {
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        out
    }

    fn events(client: &mut Client) -> Vec<Event> {
        let mut out = Vec::new();
        while let Some(event) = client.poll_event() {
            out.push(event);
        }
        out
    }

    #[test]
    fn registration_starts_with_cap_ls_then_nick_and_user() {
        let mut client = Client::new(Config::new("me"));
        client.connected();
        assert_eq!(
            drain(&mut client),
            "CAP LS 302\r\nNICK me\r\nUSER me 0 * me\r\n"
        );
    }

    #[test]
    fn a_password_precedes_nick() {
        let mut config = Config::new("me");
        config.password = Some("hunter2".to_string());
        let mut client = Client::new(config);
        client.connected();
        let sent = drain(&mut client);
        let pass = sent.find("PASS").expect("pass is sent");
        let nick = sent.find("NICK").expect("nick is sent");
        assert!(pass < nick);
    }

    #[test]
    fn answers_ping_with_the_same_token() {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b"PING :abc123\r\n");
        assert_eq!(drain(&mut client), "PONG abc123\r\n");
    }

    #[test]
    fn holds_a_partial_line_until_the_rest_arrives() {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b"PING :ab");
        assert_eq!(drain(&mut client), "");
        client.handle_bytes(b"c\r\n");
        assert_eq!(drain(&mut client), "PONG abc\r\n");
    }

    #[test]
    fn survives_a_line_that_is_not_utf8() {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b"PING :\xff\xfe\r\n");
        assert!(drain(&mut client).starts_with("PONG"));
    }

    #[test]
    fn does_not_end_negotiation_until_every_request_is_answered() {
        let mut client = Client::new(Config::new("me"));
        client.connected();
        drain(&mut client);

        client.handle_bytes(b":s CAP * LS :multi-prefix away-notify\r\n");
        let requested = drain(&mut client);
        assert!(requested.contains("CAP REQ multi-prefix"));
        assert!(requested.contains("CAP REQ away-notify"));
        assert_eq!(client.phase(), Phase::Negotiating);

        client.handle_bytes(b":s CAP * ACK :multi-prefix\r\n");
        assert_eq!(client.phase(), Phase::Negotiating);

        client.handle_bytes(b":s CAP * NAK :away-notify\r\n");
        assert_eq!(client.phase(), Phase::Registering);
        assert_eq!(drain(&mut client), "CAP END\r\n");
    }

    #[test]
    fn waits_for_the_last_line_of_a_multiline_cap_ls() {
        let mut client = Client::new(Config::new("me"));
        client.connected();
        drain(&mut client);

        client.handle_bytes(b":s CAP * LS * :multi-prefix\r\n");
        assert_eq!(
            drain(&mut client),
            "",
            "a continuation must not trigger a request"
        );

        client.handle_bytes(b":s CAP * LS :away-notify\r\n");
        let requested = drain(&mut client);
        assert!(requested.contains("multi-prefix"));
        assert!(requested.contains("away-notify"));
    }

    #[test]
    fn ends_negotiation_when_the_server_offers_nothing_we_want() {
        let mut client = Client::new(Config::new("me"));
        client.connected();
        drain(&mut client);
        client.handle_bytes(b":s CAP * LS :some-cap-we-do-not-know\r\n");
        assert_eq!(drain(&mut client), "CAP END\r\n");
    }

    #[test]
    fn cap_new_requests_a_late_capability() {
        let mut client = Client::new(Config::new("me"));
        client.connected();
        drain(&mut client);
        client.handle_bytes(b":s CAP * LS :\r\n");
        drain(&mut client);
        client.handle_bytes(b":s CAP * NEW :away-notify\r\n");
        assert!(drain(&mut client).contains("CAP REQ away-notify"));
    }

    #[test]
    fn takes_the_nick_the_server_gives_us() {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s 001 me_ :Welcome\r\n");
        assert_eq!(client.nick(), "me_");
        assert_eq!(client.phase(), Phase::Registered);
        assert_eq!(
            events(&mut client),
            [Event::Registered {
                nick: "me_".to_string()
            }]
        );
    }

    #[test]
    fn reads_the_casemapping_from_isupport() {
        let mut client = Client::new(Config::new("me"));
        assert_eq!(client.casemapping(), Casemapping::Rfc1459);
        client.handle_bytes(
            b":s 005 me CASEMAPPING=ascii CHANTYPES=#^$ PREFIX=(qaohv)~&@%+ :are supported\r\n",
        );
        assert_eq!(client.casemapping(), Casemapping::Ascii);
        assert!(
            client.isupport().is_channel("^voice"),
            "obby voice channels arrive through CHANTYPES like any other"
        );
        assert_eq!(client.isupport().prefix().mode_for_char('%'), Some('h'));
        assert_eq!(
            events(&mut client)
                .into_iter()
                .map(|event| match event {
                    Event::Isupport { token, value } => (token, value),
                    other => panic!("expected only ISUPPORT events, got {other:?}"),
                })
                .collect::<Vec<_>>(),
            [
                ("CASEMAPPING".to_string(), Some("ascii".to_string())),
                ("CHANTYPES".to_string(), Some("#^$".to_string())),
                ("PREFIX".to_string(), Some("(qaohv)~&@%+".to_string())),
            ]
        );
    }

    #[test]
    fn a_negated_isupport_token_restores_the_default() {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s 005 me CASEMAPPING=ascii :are supported\r\n");
        assert_eq!(client.casemapping(), Casemapping::Ascii);
        client.handle_bytes(b":s 005 me -CASEMAPPING :are supported\r\n");
        assert_eq!(client.casemapping(), Casemapping::Rfc1459);
    }

    fn with_sasl() -> Config {
        let mut config = Config::new("me");
        config.sasl = Some(Credentials::Plain {
            username: "alice".to_string(),
            password: "hunter2".to_string(),
        });
        config
    }

    /// Negotiate up to the point where only SASL is left outstanding.
    fn negotiated(config: Config) -> Client {
        let mut client = Client::new(config);
        client.connected();
        drain(&mut client);
        client.handle_bytes(b":s CAP * LS :sasl=PLAIN,EXTERNAL\r\n");
        client.handle_bytes(b":s CAP * ACK :sasl\r\n");
        client
    }

    #[test]
    fn authenticates_before_ending_negotiation() {
        let mut client = negotiated(with_sasl());
        let sent = drain(&mut client);
        assert!(sent.contains("AUTHENTICATE PLAIN"));
        assert!(
            !sent.contains("CAP END"),
            "CAP END during SASL makes the server abort with 906 and register us unauthenticated"
        );
        assert_eq!(client.phase(), Phase::Negotiating);

        client.handle_bytes(b":s AUTHENTICATE +\r\n");
        assert_eq!(
            drain(&mut client),
            "AUTHENTICATE AGFsaWNlAGh1bnRlcjI=\r\n",
            "the PLAIN response is authzid, authcid and password, null separated"
        );
        assert_eq!(
            client.phase(),
            Phase::Negotiating,
            "still waiting on a verdict"
        );

        client.handle_bytes(b":s 900 me me!u@h alice :You are now logged in\r\n");
        client.handle_bytes(b":s 903 me :SASL authentication successful\r\n");
        assert_eq!(drain(&mut client), "CAP END\r\n");
        assert_eq!(client.phase(), Phase::Registering);
    }

    #[test]
    fn reports_the_account_it_logged_in_as() {
        let mut client = negotiated(with_sasl());
        client.handle_bytes(b":s AUTHENTICATE +\r\n");
        client.handle_bytes(b":s 900 me me!u@h alice :You are now logged in\r\n");
        client.handle_bytes(b":s 903 me :ok\r\n");
        assert_eq!(
            events(&mut client),
            [
                Event::CapAcknowledged {
                    names: alloc::vec!["sasl".to_string()]
                },
                Event::LoggedIn {
                    account: "alice".to_string()
                },
            ]
        );
    }

    #[test]
    fn a_rejected_login_still_releases_registration() {
        let mut client = negotiated(with_sasl());
        client.handle_bytes(b":s AUTHENTICATE +\r\n");
        drain(&mut client);
        client.handle_bytes(b":s 904 me :SASL authentication failed\r\n");
        assert_eq!(drain(&mut client), "CAP END\r\n");
        assert_eq!(client.phase(), Phase::Registering);
        assert!(events(&mut client).contains(&Event::SaslFailed {
            reason: SaslFailure::Rejected
        }));
    }

    #[test]
    fn skips_authentication_when_no_mechanism_is_shared() {
        let mut client = Client::new(with_sasl());
        client.connected();
        drain(&mut client);
        client.handle_bytes(b":s CAP * LS :sasl=SCRAM-SHA-256\r\n");
        client.handle_bytes(b":s CAP * ACK :sasl\r\n");
        let sent = drain(&mut client);
        assert!(!sent.contains("AUTHENTICATE"));
        assert!(sent.contains("CAP END"));
        assert!(events(&mut client).contains(&Event::SaslFailed {
            reason: SaslFailure::NoSharedMechanism
        }));
    }

    #[test]
    fn does_not_authenticate_without_credentials() {
        let mut client = negotiated(Config::new("me"));
        let sent = drain(&mut client);
        assert!(!sent.contains("AUTHENTICATE"));
        assert!(sent.ends_with("CAP END\r\n"));
    }

    #[test]
    fn walks_the_fallback_nicks_then_appends_underscores() {
        let mut config = Config::new("me");
        config.alt_nicks = alloc::vec!["me2".to_string(), "me3".to_string()];
        let mut client = Client::new(config);
        client.connected();
        drain(&mut client);

        client.handle_bytes(b":s 433 * me :Nickname is already in use\r\n");
        assert_eq!(drain(&mut client), "NICK me2\r\n");
        client.handle_bytes(b":s 433 * me2 :Nickname is already in use\r\n");
        assert_eq!(drain(&mut client), "NICK me3\r\n");
        client.handle_bytes(b":s 433 * me3 :Nickname is already in use\r\n");
        assert_eq!(drain(&mut client), "NICK me3_\r\n", "the list is exhausted");
        assert_eq!(client.nick(), "me3_");
    }

    #[test]
    fn a_rename_refused_after_registration_is_the_hosts_problem() {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s 001 me :Welcome\r\n");
        events(&mut client);
        client.handle_bytes(b":s 433 me taken :Nickname is already in use\r\n");
        assert_eq!(
            drain(&mut client),
            "",
            "the engine did not ask for this rename"
        );
        assert!(matches!(client.poll_event(), Some(Event::Raw { .. })));
    }

    #[test]
    fn surfaces_a_line_it_does_not_model() {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s 375 me :- message of the day -\r\n");
        let Some(Event::Raw { message }) = client.poll_event() else {
            panic!("an unmodelled line must still reach the host");
        };
        assert!(message.is("375"));
    }

    #[test]
    fn a_message_reaches_the_model_through_the_whole_path() {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s 001 me :Welcome\r\n");
        client.handle_bytes(b":s 005 me CHANTYPES=# :are supported\r\n");
        client.handle_bytes(b":me!u@h JOIN #obby\r\n");
        client.handle_bytes(b"@msgid=x1 :bob!u@h PRIVMSG #obby :hello\r\n");

        let folded = client.isupport().fold("#obby");
        let channel = client.model().channel(&folded).expect("we joined it");
        assert_eq!(channel.log.len(), 1);
        assert_eq!(
            channel.log.get("x1").map(|m| m.text.as_str()),
            Some("hello")
        );
        assert!(events(&mut client).iter().any(|event| matches!(
            event,
            Event::Changed {
                change: Change::Message { .. }
            }
        )));
    }

    #[test]
    fn registering_tells_the_model_who_we_are() {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s 001 me_ :Welcome\r\n");
        assert_eq!(
            client.model().me.nick,
            "me_",
            "the model has to agree with the connection about our own nick, or every own-message check is wrong"
        );
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    fn registered() -> Client {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s 001 me :Welcome\r\n");
        client.handle_bytes(b":s 005 me CHANTYPES=# :are supported\r\n");
        client.handle_bytes(b":me!u@h JOIN #obby\r\n");
        while client.poll_event().is_some() {}
        client
    }

    fn texts(client: &Client) -> Vec<String> {
        let folded = client.isupport().fold("#obby");
        client
            .model()
            .channel(&folded)
            .expect("channel")
            .log
            .iter()
            .map(|m| m.text.clone())
            .collect()
    }

    #[test]
    fn a_batched_line_is_held_until_the_batch_closes() {
        let mut client = registered();
        client.handle_bytes(b":s BATCH +h chathistory #obby\r\n");
        client.handle_bytes(b"@batch=h;msgid=1 :bob!u@h PRIVMSG #obby :held\r\n");
        assert!(
            texts(&client).is_empty(),
            "history must land as one block, not trickle in"
        );

        client.handle_bytes(b":s BATCH -h\r\n");
        assert_eq!(texts(&client), ["held"]);
    }

    #[test]
    fn history_is_marked_as_replayed_and_live_traffic_is_not() {
        let mut client = registered();
        client.handle_bytes(b"@msgid=live :bob!u@h PRIVMSG #obby :now\r\n");
        client.handle_bytes(b":s BATCH +h chathistory #obby\r\n");
        client.handle_bytes(b"@batch=h;msgid=old :bob!u@h PRIVMSG #obby :before\r\n");
        client.handle_bytes(b":s BATCH -h\r\n");

        let folded = client.isupport().fold("#obby");
        let channel = client.model().channel(&folded).expect("channel");
        assert_eq!(channel.log.get("live").map(|m| m.historical), Some(false));
        assert_eq!(channel.log.get("old").map(|m| m.historical), Some(true));
    }

    #[test]
    fn a_batch_nested_inside_history_is_still_history() {
        let mut client = registered();
        client.handle_bytes(b":s BATCH +outer chathistory #obby\r\n");
        client.handle_bytes(b"@batch=outer :s BATCH +inner netsplit a.net b.net\r\n");
        client.handle_bytes(b"@batch=inner;msgid=n1 :bob!u@h PRIVMSG #obby :inside\r\n");
        client.handle_bytes(b":s BATCH -inner\r\n");
        client.handle_bytes(b":s BATCH -outer\r\n");

        let folded = client.isupport().fold("#obby");
        let channel = client.model().channel(&folded).expect("channel");
        assert_eq!(
            channel.log.get("n1").map(|m| m.historical),
            Some(true),
            "the ancestor chain decides, not the innermost batch type"
        );
    }

    #[test]
    fn a_line_tagged_for_an_unknown_batch_is_still_delivered() {
        let mut client = registered();
        client.handle_bytes(b"@batch=never-opened;msgid=x :bob!u@h PRIVMSG #obby :orphan\r\n");
        assert_eq!(
            texts(&client),
            ["orphan"],
            "dropping it would lose a message whenever we miss a BATCH open"
        );
    }

    #[test]
    fn history_replayed_twice_is_stored_once() {
        let mut client = registered();
        for _ in 0..2 {
            client.handle_bytes(b":s BATCH +h chathistory #obby\r\n");
            client.handle_bytes(b"@batch=h;msgid=dup :bob!u@h PRIVMSG #obby :again\r\n");
            client.handle_bytes(b":s BATCH -h\r\n");
        }
        assert_eq!(texts(&client), ["again"]);
    }
}

#[cfg(test)]
mod time_tests {
    use super::*;

    fn at(ms: u64) -> Now {
        Now {
            monotonic_ms: ms,
            unix_ms: 1_700_000_000_000 + ms,
        }
    }

    fn sent(client: &mut Client) -> String {
        let mut out = String::new();
        while let Some(bytes) = client.poll_transmit() {
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        out
    }

    fn events(client: &mut Client) -> Vec<Event> {
        let mut out = Vec::new();
        while let Some(event) = client.poll_event() {
            out.push(event);
        }
        out
    }

    #[test]
    fn a_quiet_link_gets_a_keepalive_ping() {
        let mut client = Client::new(Config::new("me"));
        client.connected();
        sent(&mut client);

        client.tick(at(PING_KEEPALIVE_MS - 1));
        assert_eq!(sent(&mut client), "", "nothing is due yet");

        client.tick(at(PING_KEEPALIVE_MS));
        assert_eq!(sent(&mut client), "PING me\r\n");
    }

    #[test]
    fn a_ping_with_no_answer_declares_the_link_dead() {
        let mut client = Client::new(Config::new("me"));
        client.connected();
        client.tick(at(PING_KEEPALIVE_MS));
        events(&mut client);

        client.tick(at(PING_KEEPALIVE_MS + DEAD_LINK_MS - 1));
        assert!(events(&mut client).is_empty());

        client.tick(at(PING_KEEPALIVE_MS + DEAD_LINK_MS));
        assert_eq!(events(&mut client), [Event::LinkDead]);
    }

    #[test]
    fn any_traffic_at_all_proves_the_link_is_alive() {
        let mut client = Client::new(Config::new("me"));
        client.connected();
        client.tick(at(PING_KEEPALIVE_MS));
        sent(&mut client);

        client.handle_bytes(b":s PONG me :me\r\n");
        client.tick(at(PING_KEEPALIVE_MS + DEAD_LINK_MS));
        assert!(
            !events(&mut client).contains(&Event::LinkDead),
            "a pong clears the dead-link deadline"
        );
    }

    #[test]
    fn the_next_deadline_is_reported_so_a_host_can_sleep_exactly() {
        let mut client = Client::new(Config::new("me"));
        assert_eq!(
            client.poll_timeout(),
            None,
            "nothing is pending before connecting"
        );
        client.connected();
        assert_eq!(client.poll_timeout(), Some(PING_KEEPALIVE_MS));
    }

    #[test]
    fn a_dropped_link_keeps_the_model_and_backs_off() {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s 001 me :Welcome\r\n");
        client.handle_bytes(b":s 005 me CHANTYPES=# :are supported\r\n");
        client.handle_bytes(b":me!u@h JOIN #obby\r\n");
        client.handle_bytes(b"@msgid=x :bob!u@h PRIVMSG #obby :hello\r\n");
        events(&mut client);

        client.disconnected();
        let folded = client.isupport().fold("#obby");
        assert!(
            client.model().channel(&folded).is_some(),
            "losing the link must not lose the scrollback"
        );

        let first = events(&mut client);
        assert!(matches!(first.as_slice(), [Event::Reconnect { .. }]));

        client.connected();
        client.disconnected();
        assert!(
            matches!(events(&mut client).as_slice(), [Event::Reconnect { after_ms }] if *after_ms == ReconnectBackoff::DEFAULT_BASE_MS),
            "a successful connection resets the backoff"
        );
    }

    #[test]
    fn backing_off_doubles_while_the_link_stays_down() {
        let mut client = Client::new(Config::new("me"));
        let mut delays = Vec::new();
        for _ in 0..3 {
            client.disconnected();
            for event in events(&mut client) {
                if let Event::Reconnect { after_ms } = event {
                    delays.push(after_ms);
                }
            }
        }
        assert!(
            delays.windows(2).all(|pair| pair[1] > pair[0]),
            "each attempt waits longer than the last, got {delays:?}"
        );
    }

    #[test]
    fn a_command_is_labelled_only_when_the_server_can_correlate_it() {
        let mut client = Client::new(Config::new("me"));
        assert_eq!(
            client.send_labeled(&Message::new("WHO", ["#obby"])),
            None,
            "labelling a server that never agreed to it only confuses it"
        );
        assert_eq!(sent(&mut client), "WHO #obby\r\n");

        client.connected();
        sent(&mut client);
        client.handle_bytes(b":s CAP * LS :labeled-response\r\n");
        client.handle_bytes(b":s CAP * ACK :labeled-response\r\n");
        sent(&mut client);

        let label = client
            .send_labeled(&Message::new("WHO", ["#obby"]))
            .expect("the capability is in force");
        assert_eq!(
            sent(&mut client),
            alloc::format!("@label={label} WHO #obby\r\n")
        );
    }

    #[test]
    fn a_labelled_command_that_is_never_answered_times_out() {
        let mut client = Client::new(Config::new("me"));
        client.connected();
        client.handle_bytes(b":s CAP * LS :labeled-response\r\n");
        client.handle_bytes(b":s CAP * ACK :labeled-response\r\n");
        client.send_labeled(&Message::new("WHO", ["#obby"]));
        events(&mut client);

        client.tick(at(DEFAULT_TIMEOUT_MS));
        assert_eq!(
            events(&mut client),
            [Event::CommandTimedOut {
                command: "WHO".to_string()
            }]
        );
    }

    #[test]
    fn an_answered_command_does_not_time_out() {
        let mut client = Client::new(Config::new("me"));
        client.connected();
        client.handle_bytes(b":s CAP * LS :labeled-response\r\n");
        client.handle_bytes(b":s CAP * ACK :labeled-response\r\n");
        let label = client
            .send_labeled(&Message::new("WHO", ["#obby"]))
            .expect("labelled");
        client.handle_bytes(alloc::format!("@label={label} :s 315 me #obby :End\r\n").as_bytes());
        events(&mut client);

        client.tick(at(DEFAULT_TIMEOUT_MS));
        assert!(
            !events(&mut client)
                .iter()
                .any(|e| matches!(e, Event::CommandTimedOut { .. }))
        );
    }
}

/// Regression tests for defects found in review. Each names the input that used to break it.
#[cfg(test)]
mod hostile_input_tests {
    use super::*;

    fn registered() -> Client {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s 001 me :Welcome\r\n");
        client.handle_bytes(b":s 005 me CHANTYPES=# STATUSMSG=@+ PREFIX=(ov)@+ :are supported\r\n");
        while client.poll_event().is_some() {}
        client
    }

    #[test]
    fn a_stream_with_no_newline_does_not_grow_without_bound() {
        let mut client = Client::new(Config::new("me"));
        for _ in 0..32 {
            client.handle_bytes(&[b'A'; 8192]);
        }
        assert!(
            client.dropped_lines() > 0,
            "an oversized line has to be given up on, not accumulated"
        );

        client.handle_bytes(b"\r\n:s 001 me :Welcome\r\n");
        assert_eq!(
            client.phase(),
            Phase::Registered,
            "the connection keeps working once the oversized line is behind us"
        );
    }

    #[test]
    fn an_absurd_server_time_does_not_take_the_process_down() {
        let mut client = registered();
        client.handle_bytes(
            b"@time=99999999999999999-01-01T00:00:00.000Z :bob!u@h PRIVMSG me :hi\r\n",
        );
        let folded = client.isupport().fold("bob");
        assert_eq!(
            client.model().conversation(&folded).map(|q| q.log.len()),
            Some(1),
            "an unusable timestamp falls back to our own, it does not panic or lose the message"
        );
    }

    #[test]
    fn a_channel_we_never_joined_is_never_invented() {
        let mut client = registered();
        client.handle_bytes(b":bob!u@h JOIN #ghost\r\n");
        client.handle_bytes(b":bob!u@h TOPIC #ghost :not ours\r\n");
        client.handle_bytes(b":s 353 me = #ghost :@bob\r\n");
        client.handle_bytes(b":s 332 me #ghost :still not ours\r\n");
        client.handle_bytes(b":bob!u@h MODE #ghost +m\r\n");
        assert_eq!(
            client.model().channels().count(),
            0,
            "a server naming channels we are not in would otherwise grow the model without limit"
        );
    }

    #[test]
    fn a_status_message_still_belongs_to_its_channel() {
        let mut client = registered();
        client.handle_bytes(b":me!u@h JOIN #obby\r\n");
        client.handle_bytes(b"@msgid=s1 :bob!u@h PRIVMSG @#obby :ops only\r\n");

        let channel = client.isupport().fold("#obby");
        assert_eq!(
            client.model().channel(&channel).map(|c| c.log.len()),
            Some(1),
            "a message to the operators of a channel is still a message in that channel"
        );
        assert!(
            client
                .model()
                .conversation(&client.isupport().fold("bob"))
                .is_none(),
            "it must not become a private conversation"
        );
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;
    use crate::command::Typing;

    fn ready(caps: &[u8]) -> Client {
        let mut client = Client::new(Config::new("me"));
        client.connected();
        client.handle_bytes(b":s CAP * LS :echo-message labeled-response\r\n");
        client.handle_bytes(caps);
        client.handle_bytes(b":s 001 me :Welcome\r\n");
        client.handle_bytes(b":s 005 me CHANTYPES=# :are supported\r\n");
        client.handle_bytes(b":me!u@h JOIN #obby\r\n");
        while client.poll_event().is_some() {}
        while client.poll_transmit().is_some() {}
        client
    }

    fn sent(client: &mut Client) -> String {
        let mut out = String::new();
        while let Some(bytes) = client.poll_transmit() {
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        out
    }

    fn log_len(client: &Client, channel: &str) -> usize {
        client
            .model()
            .channel(&client.isupport().fold(channel))
            .map_or(0, |c| c.log.len())
    }

    #[test]
    fn without_echo_message_we_record_our_own_message_ourselves() {
        let mut client = ready(b":s CAP * NAK :echo-message\r\n");
        client.command(Command::Message {
            target: "#obby".to_string(),
            text: "hello".to_string(),
        });
        assert!(sent(&mut client).contains("PRIVMSG #obby hello"));
        assert_eq!(
            log_len(&client, "#obby"),
            1,
            "no echo is coming, so nothing else will record it"
        );
        let channel = client
            .model()
            .channel(&client.isupport().fold("#obby"))
            .expect("channel");
        assert!(channel.log.last().expect("message").own);
    }

    #[test]
    fn with_echo_message_we_wait_for_the_server_rather_than_double_up() {
        let mut client = ready(b":s CAP * ACK :echo-message\r\n");
        client.command(Command::Message {
            target: "#obby".to_string(),
            text: "hello".to_string(),
        });
        assert_eq!(
            log_len(&client, "#obby"),
            0,
            "recording it now and again on the echo is how a message appears twice"
        );

        client.handle_bytes(b"@msgid=e1 :me!u@h PRIVMSG #obby :hello\r\n");
        assert_eq!(log_len(&client, "#obby"), 1);
    }

    #[test]
    fn an_action_is_wrapped_as_ctcp_and_read_back_as_one() {
        let mut client = ready(b":s CAP * NAK :echo-message\r\n");
        client.command(Command::Action {
            target: "#obby".to_string(),
            text: "waves".to_string(),
        });
        assert!(sent(&mut client).contains("\u{1}ACTION waves\u{1}"));
        let channel = client
            .model()
            .channel(&client.isupport().fold("#obby"))
            .expect("channel");
        let message = channel.log.last().expect("message");
        assert_eq!(
            message.kind,
            crate::model::MessageKind::Ctcp {
                command: "ACTION".to_string()
            }
        );
        assert_eq!(message.text, "waves");
    }

    #[test]
    fn joining_and_parting_go_out_as_written() {
        let mut client = ready(b":s CAP * NAK :echo-message\r\n");
        client.command(Command::Join {
            channel: "#secret".to_string(),
            key: Some("hunter2".to_string()),
        });
        client.command(Command::Part {
            channel: "#obby".to_string(),
            reason: Some("see you".to_string()),
        });
        let sent = sent(&mut client);
        assert!(sent.contains("JOIN #secret hunter2"));
        assert!(
            sent.contains("PART #obby :see you"),
            "a reason with a space needs its marker"
        );
    }

    #[test]
    fn clearing_a_topic_is_distinguishable_from_asking_for_one() {
        let mut client = ready(b":s CAP * NAK :echo-message\r\n");
        client.command(Command::Topic {
            channel: "#obby".to_string(),
            topic: None,
        });
        assert!(
            sent(&mut client).contains("TOPIC #obby :"),
            "an empty trailing parameter clears the topic; omitting it would only ask what it is"
        );
    }

    #[test]
    fn a_reaction_names_the_message_it_reacts_to() {
        let mut client = ready(b":s CAP * NAK :echo-message\r\n");
        client.command(Command::React {
            target: "#obby".to_string(),
            msgid: "m1".to_string(),
            emoji: "👍".to_string(),
        });
        let sent = sent(&mut client);
        assert!(sent.contains("+draft/react=👍"));
        assert!(sent.contains("+draft/reply=m1"));
        assert!(sent.contains("TAGMSG #obby"));
    }

    #[test]
    fn typing_says_how_far_along_we_are() {
        let mut client = ready(b":s CAP * NAK :echo-message\r\n");
        client.command(Command::Typing {
            target: "#obby".to_string(),
            state: Typing::Active,
        });
        assert!(sent(&mut client).contains("+typing=active"));
    }

    #[test]
    fn asking_for_history_pages_backwards_from_a_message() {
        let mut client = ready(b":s CAP * NAK :echo-message\r\n");
        client.command(Command::History {
            target: "#obby".to_string(),
            before: None,
            limit: 50,
        });
        assert!(sent(&mut client).contains("CHATHISTORY LATEST #obby * 50"));

        client.command(Command::History {
            target: "#obby".to_string(),
            before: Some("m1".to_string()),
            limit: 50,
        });
        assert!(sent(&mut client).contains("CHATHISTORY BEFORE #obby msgid=m1 50"));
    }

    #[test]
    fn a_raw_line_that_will_not_parse_is_not_sent() {
        let mut client = ready(b":s CAP * NAK :echo-message\r\n");
        client.command(Command::Raw {
            line: String::new(),
        });
        assert_eq!(
            sent(&mut client),
            "",
            "a malformed line must not reach the wire"
        );
    }
}

#[cfg(test)]
mod resume_tests {
    use super::*;

    fn sent(client: &mut Client) -> String {
        let mut out = String::new();
        while let Some(bytes) = client.poll_transmit() {
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        out
    }

    /// A client that has been around: registered, in two channels, with history.
    fn established() -> Client {
        let mut client = Client::new(Config::new("me"));
        client.connected();
        client.handle_bytes(b":s CAP * LS :draft/chathistory\r\n");
        client.handle_bytes(b":s CAP * ACK :draft/chathistory\r\n");
        client.handle_bytes(b":s 001 me :Welcome\r\n");
        client.handle_bytes(b":s 005 me CHANTYPES=# :are supported\r\n");
        client.handle_bytes(b":me!u@h JOIN #obby\r\n");
        client.handle_bytes(b":me!u@h JOIN #other\r\n");
        client.handle_bytes(b":bob!u@h JOIN #obby\r\n");
        client.handle_bytes(b"@msgid=last :bob!u@h PRIVMSG #obby :before the drop\r\n");
        while client.poll_event().is_some() {}
        sent(&mut client);
        client
    }

    #[test]
    fn a_reconnect_rejoins_every_channel_and_asks_only_for_what_it_missed() {
        let mut client = established();
        client.disconnected();
        client.connected();
        sent(&mut client);

        client.handle_bytes(b":s CAP * LS :draft/chathistory\r\n");
        client.handle_bytes(b":s CAP * ACK :draft/chathistory\r\n");
        client.handle_bytes(b":s 001 me :Welcome back\r\n");
        let replay = sent(&mut client);

        assert!(replay.contains("JOIN #obby"));
        assert!(replay.contains("JOIN #other"));
        assert!(
            replay.contains("CHATHISTORY AFTER #obby msgid=last 100"),
            "resuming from the newest message we hold beats refetching a window, got: {replay}"
        );
        assert!(
            !replay.contains("CHATHISTORY AFTER #other"),
            "a channel we have no message for has no point to resume from"
        );
    }

    #[test]
    fn the_scrollback_survives_but_the_member_list_does_not() {
        let mut client = established();
        let folded = client.isupport().fold("#obby");
        assert_eq!(
            client.model().channel(&folded).map(|c| c.members.len()),
            Some(1)
        );

        client.disconnected();

        let channel = client
            .model()
            .channel(&folded)
            .expect("the channel outlives the link");
        assert_eq!(channel.log.len(), 1, "the messages happened, so they stay");
        assert!(
            channel.members.is_empty(),
            "who is in a channel is only knowable while connected, so a stale list is a lie"
        );
    }

    #[test]
    fn a_first_connection_replays_nothing() {
        let mut client = Client::new(Config::new("me"));
        client.connected();
        sent(&mut client);
        client.handle_bytes(b":s 001 me :Welcome\r\n");
        assert_eq!(sent(&mut client), "", "there is nothing to resume into");
    }

    #[test]
    fn capabilities_are_renegotiated_rather_than_assumed_to_survive() {
        let mut client = established();
        assert!(client.caps().has("draft/chathistory"));
        client.disconnected();
        assert!(
            !client.caps().has("draft/chathistory"),
            "the new link is a new negotiation; assuming otherwise sends commands the server never agreed to"
        );
    }
}

#[cfg(test)]
mod standard_reply_tests {
    use super::*;

    fn first_reply(raw: &[u8]) -> Event {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(raw);
        client.poll_event().expect("a reply reaches the host")
    }

    #[test]
    fn a_failure_carries_its_code_and_description() {
        assert_eq!(
            first_reply(b":s FAIL JOIN CHANNEL_FULL #obby :Channel is full\r\n"),
            Event::Reply {
                severity: Severity::Fail,
                command: "JOIN".to_string(),
                code: "CHANNEL_FULL".to_string(),
                context: alloc::vec!["#obby".to_string()],
                text: "Channel is full".to_string(),
            }
        );
    }

    #[test]
    fn a_reply_with_no_context_still_parses() {
        assert_eq!(
            first_reply(b":s WARN * ACCOUNT_REQUIRED :Log in for more\r\n"),
            Event::Reply {
                severity: Severity::Warn,
                command: "*".to_string(),
                code: "ACCOUNT_REQUIRED".to_string(),
                context: Vec::new(),
                text: "Log in for more".to_string(),
            }
        );
    }

    #[test]
    fn a_note_is_reported_without_being_treated_as_an_error() {
        let Event::Reply { severity, .. } = first_reply(b":s NOTE * HELLO :Welcome\r\n") else {
            panic!("a NOTE is a standard reply");
        };
        assert_eq!(severity, Severity::Note);
    }

    #[test]
    fn a_reply_missing_its_code_is_dropped_rather_than_half_reported() {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s FAIL\r\n");
        assert!(matches!(client.poll_event(), Some(Event::Raw { .. })));
    }
}

#[cfg(test)]
mod typing_tests {
    use super::*;

    fn at(ms: u64) -> Now {
        Now {
            monotonic_ms: ms,
            unix_ms: 1_700_000_000_000 + ms,
        }
    }

    fn joined() -> Client {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s 001 me :Welcome\r\n");
        client.handle_bytes(b":s 005 me CHANTYPES=# :are supported\r\n");
        client.handle_bytes(b":me!u@h JOIN #obby\r\n");
        while client.poll_event().is_some() {}
        client
    }

    fn typing_in(client: &Client, channel: &str) -> usize {
        client
            .model()
            .channel(&client.isupport().fold(channel))
            .map_or(0, |c| c.typing.len())
    }

    #[test]
    fn a_typing_indicator_is_not_a_message() {
        let mut client = joined();
        client.handle_bytes(b"@+typing=active :bob!u@h TAGMSG #obby\r\n");
        assert_eq!(
            client
                .model()
                .channel(&client.isupport().fold("#obby"))
                .map(|c| c.log.len()),
            Some(0),
            "showing it as a message would put an empty row in the conversation"
        );
        assert_eq!(typing_in(&client, "#obby"), 1);
    }

    #[test]
    fn typing_stops_when_they_say_so() {
        let mut client = joined();
        client.handle_bytes(b"@+typing=active :bob!u@h TAGMSG #obby\r\n");
        client.handle_bytes(b"@+typing=done :bob!u@h TAGMSG #obby\r\n");
        assert_eq!(typing_in(&client, "#obby"), 0);
    }

    #[test]
    fn typing_goes_stale_on_its_own() {
        let mut client = joined();
        client.handle_bytes(b"@+typing=active :bob!u@h TAGMSG #obby\r\n");
        while client.poll_event().is_some() {}

        client.tick(at(TYPING_EXPIRY_MS - 1));
        assert_eq!(typing_in(&client, "#obby"), 1);

        client.tick(at(TYPING_EXPIRY_MS));
        assert_eq!(
            typing_in(&client, "#obby"),
            0,
            "the done that would clear it can be lost, so it must expire by itself"
        );
        assert!(
            client
                .poll_event()
                .is_some_and(|event| matches!(event, Event::Typing { typing: false, .. }))
        );
    }

    #[test]
    fn a_repeated_indicator_does_not_report_twice() {
        let mut client = joined();
        client.handle_bytes(b"@+typing=active :bob!u@h TAGMSG #obby\r\n");
        while client.poll_event().is_some() {}
        client.handle_bytes(b"@+typing=active :bob!u@h TAGMSG #obby\r\n");
        assert!(
            client.poll_event().is_none(),
            "the indicator repeats on a timer, and reporting each repeat makes it flicker"
        );
    }

    #[test]
    fn typing_at_us_belongs_to_the_conversation_with_the_sender() {
        let mut client = joined();
        client.handle_bytes(b":bob!u@h PRIVMSG me :hi\r\n");
        client.handle_bytes(b"@+typing=active :bob!u@h TAGMSG me\r\n");
        assert_eq!(
            client
                .model()
                .conversation(&client.isupport().fold("bob"))
                .map(|q| q.typing.len()),
            Some(1),
            "a conversation named after ourselves would be nobody"
        );
    }
}

#[cfg(all(test, feature = "voice"))]
mod voice_tests {
    use super::*;

    fn joined() -> Client {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s 001 me :Welcome\r\n");
        client.handle_bytes(b":s 005 me CHANTYPES=#^$ :are supported\r\n");
        client.handle_bytes(b":me!u@h JOIN ^general\r\n");
        while client.poll_event().is_some() {}
        client
    }

    #[test]
    fn signalling_is_never_a_message_in_the_channel() {
        let mut client = joined();
        client.handle_bytes(
            br#"@+obsidianirc/rtc={"type":"join","channel":"^general"} :alice!u@h TAGMSG ^general"#,
        );
        client.handle_bytes(b"\r\n");
        assert_eq!(
            client
                .model()
                .channel(&client.isupport().fold("^general"))
                .map(|c| c.log.len()),
            Some(0),
            "a room full of signalling would otherwise fill the conversation with empty rows"
        );
        assert!(
            client
                .poll_event()
                .is_some_and(|e| matches!(e, Event::Voice { .. }))
        );
    }

    #[test]
    fn a_frame_we_cannot_read_is_counted_rather_than_shown() {
        let mut client = joined();
        client.handle_bytes(b"@+obsidianirc/rtc=not-json :alice!u@h TAGMSG ^general\r\n");
        assert_eq!(
            client
                .model()
                .channel(&client.isupport().fold("^general"))
                .map(|c| c.log.len()),
            Some(0)
        );
        assert!(client.dropped_lines() > 0);
    }

    #[test]
    fn an_outbound_frame_rides_the_rtc_tag() {
        let mut client = joined();
        while client.poll_transmit().is_some() {}
        client.command(Command::Voice {
            channel: "^general".to_string(),
            payload: r#"{"type":"join","channel":"^general"}"#.to_string(),
        });
        let mut sent = String::new();
        while let Some(bytes) = client.poll_transmit() {
            sent.push_str(&String::from_utf8_lossy(&bytes));
        }
        assert!(sent.contains("+obsidianirc/rtc="));
        assert!(sent.contains("TAGMSG ^general"));
    }
}

#[cfg(test)]
mod scram_tests {
    use super::*;
    use base64::Engine as _;

    /// The published RFC 7677 exchange, so the engine is checked against the specification's own
    /// bytes rather than against our implementation agreeing with itself.
    const SERVER_FIRST: &str =
        "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";
    const SERVER_FINAL: &str = "v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=";

    fn b64(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    fn negotiated() -> Client {
        let mut config = Config::new("me");
        config.sasl = Some(Credentials::Scram {
            username: "user".to_string(),
            password: "pencil".to_string(),
            nonce: "rOprNGfwEbeRWgbNEkqO".to_string(),
        });
        let mut client = Client::new(config);
        client.connected();
        client.handle_bytes(b":s CAP * LS :sasl=SCRAM-SHA-256,PLAIN\r\n");
        client.handle_bytes(b":s CAP * ACK :sasl\r\n");
        client
    }

    fn sent(client: &mut Client) -> String {
        let mut out = String::new();
        while let Some(bytes) = client.poll_transmit() {
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        out
    }

    #[test]
    fn the_whole_exchange_runs_and_only_then_ends_negotiation() {
        let mut client = negotiated();
        assert!(sent(&mut client).contains("AUTHENTICATE SCRAM-SHA-256"));

        client.handle_bytes(b":s AUTHENTICATE +\r\n");
        assert_eq!(
            sent(&mut client),
            alloc::format!(
                "AUTHENTICATE {}\r\n",
                b64("n,,n=user,r=rOprNGfwEbeRWgbNEkqO")
            ),
        );
        assert_eq!(client.phase(), Phase::Negotiating);

        client.handle_bytes(alloc::format!(":s AUTHENTICATE {}\r\n", b64(SERVER_FIRST)).as_bytes());
        let proof = sent(&mut client);
        assert!(proof.contains(&b64(
            "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
        )));
        assert_eq!(
            client.phase(),
            Phase::Negotiating,
            "CAP END before the server has proven itself abandons the check entirely"
        );

        client.handle_bytes(alloc::format!(":s AUTHENTICATE {}\r\n", b64(SERVER_FINAL)).as_bytes());
        assert_eq!(sent(&mut client), "AUTHENTICATE +\r\n");

        client.handle_bytes(b":s 903 me :SASL authentication successful\r\n");
        assert_eq!(sent(&mut client), "CAP END\r\n");
        assert_eq!(client.phase(), Phase::Registering);
    }

    #[test]
    fn a_server_that_cannot_prove_it_knows_the_password_is_refused() {
        let mut client = negotiated();
        client.handle_bytes(b":s AUTHENTICATE +\r\n");
        client.handle_bytes(alloc::format!(":s AUTHENTICATE {}\r\n", b64(SERVER_FIRST)).as_bytes());
        sent(&mut client);
        while client.poll_event().is_some() {}

        let forged = b64("v=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=");
        client.handle_bytes(alloc::format!(":s AUTHENTICATE {forged}\r\n").as_bytes());

        let mut events = Vec::new();
        while let Some(event) = client.poll_event() {
            events.push(event);
        }
        assert!(
            events.contains(&Event::SaslFailed {
                reason: SaslFailure::ServerNotVerified
            }),
            "a server that never knew the password must not be able to convince us it did"
        );
        assert_eq!(
            client.phase(),
            Phase::Registering,
            "registration still continues, unauthenticated"
        );
    }

    #[test]
    fn a_malformed_challenge_ends_the_exchange_rather_than_hanging_it() {
        let mut client = negotiated();
        client.handle_bytes(b":s AUTHENTICATE +\r\n");
        sent(&mut client);
        client.handle_bytes(alloc::format!(":s AUTHENTICATE {}\r\n", b64("nonsense")).as_bytes());
        assert_eq!(sent(&mut client), "CAP END\r\n");
        assert_eq!(client.phase(), Phase::Registering);
    }

    #[test]
    fn plain_is_skipped_when_the_server_will_not_take_it() {
        let mut config = Config::new("me");
        config.sasl = Some(Credentials::Plain {
            username: "user".to_string(),
            password: "pencil".to_string(),
        });
        let mut client = Client::new(config);
        client.connected();
        client.handle_bytes(b":s CAP * LS :sasl=SCRAM-SHA-256\r\n");
        client.handle_bytes(b":s CAP * ACK :sasl\r\n");
        let sent = sent(&mut client);
        assert!(!sent.contains("AUTHENTICATE"));
        assert!(sent.contains("CAP END"));
    }
}

#[cfg(all(test, feature = "serde"))]
mod config_tests {
    use super::*;

    #[test]
    fn a_host_only_has_to_supply_the_nick() {
        let config: Config =
            serde_json::from_str(r#"{"nick":"me"}"#).expect("a partial config should deserialise");
        assert_eq!(config.nick, "me");
        assert_eq!(config.retention, crate::model::DEFAULT_RETENTION);
        assert!(config.alt_nicks.is_empty());
        assert!(config.sasl.is_none());
    }

    #[test]
    fn a_blank_username_is_filled_in_from_the_nick() {
        let config: Config = serde_json::from_str(r#"{"nick":"me"}"#).expect("deserialise");
        let mut client = Client::new(config);
        client.connected();
        let mut sent = String::new();
        while let Some(bytes) = client.poll_transmit() {
            sent.push_str(&String::from_utf8_lossy(&bytes));
        }
        assert!(
            sent.contains("USER me 0 * me"),
            "registering with an empty username is refused outright by some servers, got: {sent}"
        );
    }
}
