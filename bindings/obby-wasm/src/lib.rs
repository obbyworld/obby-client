//! WebAssembly bindings for `obby-client`.
//!
//! One `wasm-pack build --target web` artifact serves both the browser and Bun, so nothing here
//! may assume a DOM, a `window`, or a Node built-in. [`ObbyClient`] wraps [`Client`] and mirrors its poll/drain shape: a [`Command`] goes
//! in as a JS value, every pending [`Event`] comes out at once as a JS array, and bytes come out
//! separately from events, matching `poll_transmit` versus `poll_event` in the wrapped API.

use obby_client::{Client, Command, Config, Event, Now, Signal, Typing};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::wasm_bindgen;

/// The TypeScript definitions of everything that crosses this boundary.
///
/// Generated from the Rust types by `make ts-types`, so a shape can never drift from the engine
/// that produces it. `wasm-bindgen` copies this verbatim into the package's `.d.ts`.
#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = include_str!("types.d.ts");

#[wasm_bindgen]
unsafe extern "C" {
    /// A [`Config`], as TypeScript sees it.
    #[wasm_bindgen(typescript_type = "Config")]
    pub type ConfigValue;

    /// A [`Command`], as TypeScript sees it.
    #[wasm_bindgen(typescript_type = "Command")]
    pub type CommandValue;

    /// A [`Typing`] state, as TypeScript sees it.
    #[wasm_bindgen(typescript_type = "TypingState")]
    pub type TypingStateValue;

    /// A [`Signal`], as TypeScript sees it.
    #[wasm_bindgen(typescript_type = "VoiceSignal")]
    pub type VoiceSignalValue;

    /// Everything drained by [`ObbyClient::poll_events`], as TypeScript sees it.
    #[wasm_bindgen(typescript_type = "ObbyEvent[]")]
    pub type EventsValue;

    /// A [`obby_client::Model`], as TypeScript sees it.
    #[wasm_bindgen(typescript_type = "Model")]
    pub type ModelValue;
}

/// Serialise the way the generated types say we do.
///
/// The default serialiser hands JavaScript a `Map` for every map, `undefined` for every absent
/// option, and a `BigInt` for every 64-bit number. The definitions promise a plain object, `null`
/// and `number`, and this is what keeps that promise, so `JSON.stringify` and `Object.keys` work on
/// anything this returns.
fn to_js<T: serde::Serialize + ?Sized>(value: &T) -> Result<JsValue, serde_wasm_bindgen::Error> {
    value.serialize(&serde_wasm_bindgen::Serializer::json_compatible())
}

/// A JavaScript number of milliseconds, as the engine's `u64`.
///
/// JavaScript has one number type, and `wasm-bindgen` would otherwise put a `BigInt` in every
/// signature that carries a millisecond, which a host would have to convert at every call. A
/// millisecond count is exact in an `f64` for the next quarter of a million years.
fn as_millis(value: f64) -> u64 {
    // a host that hands us a NaN, an infinity or a negative gets the epoch, which is wrong but
    // bounded; a panic in the middle of someone's render loop is not
    if value.is_finite() && value >= 0.0 {
        value.trunc() as u64
    } else {
        0
    }
}

/// The engine's `u64` of milliseconds, as a JavaScript number.
fn as_js_number(value: u64) -> f64 {
    value as f64
}

/// One connection, wrapped for JavaScript.
///
/// Every method mirrors one on [`Client`]. None of them can panic: a bad argument comes back as a
/// rejected `Result`, which `wasm-bindgen` turns into a thrown JS error, because a panic inside a
/// WebAssembly module poisons it for the rest of the host's lifetime, with no way to recover.
///
/// @example
/// ```ts
/// import init, { ObbyClient } from "obby-client";
///
/// await init();
/// const client = new ObbyClient({ nick: "mynick" });
/// const socket = new WebSocket("wss://irc.example.org/webirc");
/// socket.binaryType = "arraybuffer";
///
/// const flush = () => {
///   for (let bytes; (bytes = client.pollTransmit()); ) socket.send(bytes);
/// };
///
/// socket.onopen = () => {
///   client.handleConnected();
///   flush();
/// };
///
/// socket.onmessage = (message) => {
///   client.handleBytes(new Uint8Array(message.data as ArrayBuffer));
///   client.tick(performance.now(), Date.now());
///
///   for (const event of client.pollEvents()) {
///     if (event.type === "registered") {
///       client.join("#obby");
///       client.sendMessage("#obby", `hello, I am ${event.nick}`);
///     }
///   }
///   flush();
/// };
/// ```
#[wasm_bindgen]
pub struct ObbyClient {
    inner: Client,
}

#[wasm_bindgen]
impl ObbyClient {
    /// Build an engine that has not connected yet. Nothing is written until [`Self::handle_connected`].
    ///
    /// `config` is a JS object with the same shape as [`Config`]. Only `nick` is required; every
    /// other field has a default.
    #[wasm_bindgen(constructor)]
    pub fn new(config: ConfigValue) -> Result<ObbyClient, JsValue> {
        let config: Config = serde_wasm_bindgen::from_value(config.into())?;
        Ok(Self {
            inner: Client::new(config),
        })
    }

    /// Tell the engine the transport is up. Queues the registration burst.
    #[wasm_bindgen(js_name = handleConnected)]
    pub fn handle_connected(&mut self) {
        self.inner.handle_connected();
    }

    /// Tell the engine its transport died. The model survives, so a reconnect can resume from it.
    #[wasm_bindgen(js_name = handleDisconnected)]
    pub fn handle_disconnected(&mut self) {
        self.inner.handle_disconnected();
    }

    /// Advance the clock. `monotonicMs` drives every deadline; `unixMs` only stamps a message the
    /// server did not stamp itself with `server-time`.
    pub fn tick(&mut self, monotonic_ms: f64, unix_ms: f64) {
        self.inner.tick(Now {
            monotonic_ms: as_millis(monotonic_ms),
            unix_ms: as_millis(unix_ms),
        });
    }

    /// When [`Self::tick`] next has something to do, as a monotonic instant, or `undefined` when
    /// nothing is scheduled. A host can set one timer for exactly this instant instead of polling
    /// on an interval.
    #[wasm_bindgen(js_name = pollTimeout)]
    pub fn poll_timeout(&self) -> Option<f64> {
        self.inner.poll_timeout().map(as_js_number)
    }

    /// Join a channel, with its key when it has one.
    ///
    /// @example
    /// ```ts
    /// client.join("#obby");
    /// client.join("#staff", "hunter2");
    /// ```
    pub fn join(&mut self, channel: String, key: Option<String>) {
        self.apply(Command::Join { channel, key });
    }

    /// Leave a channel, with a reason the others in it see.
    pub fn part(&mut self, channel: String, reason: Option<String>) {
        self.apply(Command::Part { channel, reason });
    }

    /// Say something to a channel or a person.
    ///
    /// @example
    /// ```ts
    /// client.sendMessage("#obby", "hello there");
    /// client.sendMessage("alice", "a private word");
    /// ```
    #[wasm_bindgen(js_name = sendMessage)]
    pub fn send_message(&mut self, target: String, text: String) {
        self.apply(Command::SendMessage { target, text });
    }

    /// Send a notice, which by convention must never be auto-replied to.
    #[wasm_bindgen(js_name = sendNotice)]
    pub fn send_notice(&mut self, target: String, text: String) {
        self.apply(Command::SendNotice { target, text });
    }

    /// Send a `CTCP ACTION`, the third-person form.
    #[wasm_bindgen(js_name = sendAction)]
    pub fn send_action(&mut self, target: String, text: String) {
        self.apply(Command::SendAction { target, text });
    }

    /// Change our nick.
    #[wasm_bindgen(js_name = setNick)]
    pub fn set_nick(&mut self, nick: String) {
        self.apply(Command::SetNick { nick });
    }

    /// Set a channel's topic, or ask for the current one by passing nothing.
    #[wasm_bindgen(js_name = setTopic)]
    pub fn set_topic(&mut self, channel: String, topic: Option<String>) {
        self.apply(Command::SetTopic { channel, topic });
    }

    /// Go away with a message, or come back by passing nothing.
    #[wasm_bindgen(js_name = setAway)]
    pub fn set_away(&mut self, message: Option<String>) {
        self.apply(Command::SetAway { message });
    }

    /// Tell a target we are composing, paused, or done.
    #[wasm_bindgen(js_name = setTyping)]
    pub fn set_typing(&mut self, target: String, state: TypingStateValue) -> Result<(), JsValue> {
        let state: Typing = serde_wasm_bindgen::from_value(state.into())?;
        self.apply(Command::SetTyping { target, state });
        Ok(())
    }

    /// React to a message with an emoji.
    #[wasm_bindgen(js_name = addReaction)]
    pub fn add_reaction(&mut self, target: String, msgid: String, emoji: String) {
        self.apply(Command::AddReaction {
            target,
            msgid,
            emoji,
        });
    }

    /// Take one of our reactions back.
    #[wasm_bindgen(js_name = removeReaction)]
    pub fn remove_reaction(&mut self, target: String, msgid: String, emoji: String) {
        self.apply(Command::RemoveReaction {
            target,
            msgid,
            emoji,
        });
    }

    /// Ask the server to redact a message.
    #[wasm_bindgen(js_name = redactMessage)]
    pub fn redact_message(&mut self, target: String, msgid: String, reason: Option<String>) {
        self.apply(Command::RedactMessage {
            target,
            msgid,
            reason,
        });
    }

    /// Move our read marker in a target, at a time in milliseconds since the Unix epoch.
    #[wasm_bindgen(js_name = markRead)]
    pub fn mark_read(&mut self, target: String, at_ms: f64) {
        self.apply(Command::MarkRead {
            target,
            at_ms: as_millis(at_ms),
        });
    }

    /// Ask for older messages in a target, before the message with this id.
    #[wasm_bindgen(js_name = fetchHistory)]
    pub fn fetch_history(&mut self, target: String, before_msgid: Option<String>, limit: u16) {
        self.apply(Command::FetchHistory {
            target,
            before_msgid,
            limit,
        });
    }

    /// Set one of our own metadata keys, or clear it by passing nothing.
    #[wasm_bindgen(js_name = setMetadata)]
    pub fn set_metadata(&mut self, key: String, value: Option<String>) {
        self.apply(Command::SetMetadata { key, value });
    }

    /// Subscribe to the metadata keys we want told about.
    #[wasm_bindgen(js_name = subscribeMetadata)]
    pub fn subscribe_metadata(&mut self, keys: Vec<String>) {
        self.apply(Command::SubscribeMetadata { keys });
    }

    /// Ask the server everything it will say about someone.
    pub fn whois(&mut self, nick: String) {
        self.apply(Command::Whois { nick });
    }

    /// Rename a channel, keeping everyone in it and everything said in it.
    #[wasm_bindgen(js_name = renameChannel)]
    pub fn rename_channel(&mut self, channel: String, new_name: String, reason: Option<String>) {
        self.apply(Command::RenameChannel {
            channel,
            new_name,
            reason,
        });
    }

    /// Make an invitation link to a channel, or to the network when no channel is named.
    #[wasm_bindgen(js_name = createInviteLink)]
    pub fn create_invite_link(&mut self, channel: Option<String>, description: Option<String>) {
        self.apply(Command::CreateInviteLink {
            channel,
            description,
        });
    }

    /// Ask for the invitation links we have made.
    #[wasm_bindgen(js_name = listInviteLinks)]
    pub fn list_invite_links(&mut self) {
        self.apply(Command::ListInviteLinks);
    }

    /// Withdraw an invitation link.
    #[wasm_bindgen(js_name = deleteInviteLink)]
    pub fn delete_invite_link(&mut self, share_id: String) {
        self.apply(Command::DeleteInviteLink { share_id });
    }

    /// Redeem an invitation code. Only before registering, which is the point of it.
    #[wasm_bindgen(js_name = redeemInviteCode)]
    pub fn redeem_invite_code(&mut self, code: String) {
        self.apply(Command::RedeemInviteCode { code });
    }

    /// Mint a bearer token for one of the network's services, such as its file host.
    #[wasm_bindgen(js_name = generateToken)]
    pub fn generate_token(&mut self, service: String) {
        self.apply(Command::GenerateToken { service });
    }

    /// Watch nicks, so we hear when they come online.
    #[wasm_bindgen(js_name = watchNicks)]
    pub fn watch_nicks(&mut self, nicks: Vec<String>) {
        self.apply(Command::WatchNicks { nicks });
    }

    /// Stop watching nicks.
    #[wasm_bindgen(js_name = unwatchNicks)]
    pub fn unwatch_nicks(&mut self, nicks: Vec<String>) {
        self.apply(Command::UnwatchNicks { nicks });
    }

    /// Send one voice signalling frame to a room.
    #[wasm_bindgen(js_name = sendVoiceSignal)]
    pub fn send_voice_signal(
        &mut self,
        channel: String,
        signal: VoiceSignalValue,
    ) -> Result<(), JsValue> {
        let signal: Signal = serde_wasm_bindgen::from_value(signal.into())?;
        self.apply(Command::SendVoiceSignal { channel, signal });
        Ok(())
    }

    /// Leave the server, with a reason the others see.
    pub fn quit(&mut self, reason: Option<String>) {
        self.apply(Command::Quit { reason });
    }

    /// Send one raw protocol line, for anything this API does not name.
    #[wasm_bindgen(js_name = sendRawLine)]
    pub fn send_raw_line(&mut self, line: String) {
        self.apply(Command::SendRawLine { line });
    }

    /// Do anything, as a [`Command`] object.
    ///
    /// Every command also has a method of its own, such as [`Self::join`]; this is the one call
    /// that takes a command a host built itself.
    ///
    /// Anything the engine cannot read throws, rather than going quietly missing.
    ///
    /// @example
    /// ```ts
    /// const command: Command = { type: "join", channel: "#obby", key: null };
    /// client.command(command);
    /// client.command({ type: "set_topic", channel: "#obby", topic: "the new topic" });
    /// ```
    pub fn command(&mut self, command: CommandValue) -> Result<(), JsValue> {
        let command: Command = serde_wasm_bindgen::from_value(command.into())?;
        self.apply(command);
        Ok(())
    }

    /// Feed whatever the transport read. Partial lines are held until the rest arrives.
    #[wasm_bindgen(js_name = handleBytes)]
    pub fn handle_bytes(&mut self, data: &[u8]) {
        self.inner.handle_bytes(data);
    }

    /// Bytes the host should write to the transport, or `undefined` when there are none.
    ///
    /// One chunk per call, unlike [`Self::poll_events`]: a chunk is already the smallest unit a
    /// socket writes, so there is nothing to gain from batching it, and a growing [`Event`] never
    /// has to carry it.
    #[wasm_bindgen(js_name = pollTransmit)]
    pub fn poll_transmit(&mut self) -> Option<Vec<u8>> {
        self.inner.poll_transmit()
    }

    /// Every event the engine has queued since the last call, as a JS array.
    ///
    /// Draining a batch instead of one event per call is what keeps this binding cheap on every
    /// target this core is bound into: a call across the WebAssembly boundary costs the same
    /// whether it carries one event or a hundred, so paying that cost once per drain rather than
    /// once per event is what actually saves work.
    ///
    /// @example
    /// ```ts
    /// for (const event of client.pollEvents()) {
    ///   switch (event.type) {
    ///     case "registered":
    ///       console.log(`registered as ${event.nick}`);
    ///       break;
    ///     case "model_changed":
    ///       if (event.change.type === "message_added") render(event.change.target);
    ///       break;
    ///     case "server_reply":
    ///       console.log(event.severity, event.code, event.text);
    ///       break;
    ///   }
    /// }
    /// ```
    #[wasm_bindgen(js_name = pollEvents)]
    pub fn poll_events(&mut self) -> Result<EventsValue, JsValue> {
        Ok(to_js(&self.drain_events())?.unchecked_into())
    }

    /// Everything the connection knows, as a JS value: channels, members, conversations and
    /// messages. For a host that only wants the model, not a diff of what changed.
    pub fn model(&self) -> Result<ModelValue, JsValue> {
        Ok(to_js(self.inner.model())?.unchecked_into())
    }
}

impl ObbyClient {
    fn apply(&mut self, command: Command) {
        self.inner.command(command);
    }

    fn drain_events(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(event) = self.inner.poll_event() {
            events.push(event);
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_client() -> ObbyClient {
        ObbyClient {
            inner: Client::new(Config::new("me")),
        }
    }

    // `JsValue` cannot be constructed off the wasm32 target: wasm-bindgen's externref shims panic
    // there by design, since there is no JS engine to hold the value. Everything below either
    // calls the exported methods directly with non-`JsValue` arguments, which is safe on any
    // target, or exercises the same serde contract `serde_wasm_bindgen` relies on through
    // `serde_json`. The `JsValue`-carrying methods (`new`, `command`, `pollEvents`, `model`) are
    // only exercised for real in a browser or through `wasm-pack test`.

    #[test]
    fn connected_queues_the_registration_burst() {
        let mut client = new_client();
        client.handle_connected();
        let first = client
            .poll_transmit()
            .expect("registration starts on connect");
        assert!(first.starts_with(b"CAP LS 302"));
    }

    #[test]
    fn poll_transmit_drains_one_chunk_at_a_time() {
        let mut client = new_client();
        client.handle_connected();
        assert!(client.poll_transmit().is_some(), "CAP LS");
        assert!(client.poll_transmit().is_some(), "NICK");
        assert!(client.poll_transmit().is_some(), "USER");
        assert!(client.poll_transmit().is_none());
    }

    #[test]
    fn handle_bytes_feeds_a_line_to_the_engine() {
        let mut client = new_client();
        client.handle_bytes(b"PING :abc\r\n");
        let reply = client.poll_transmit().expect("a PING gets a PONG");
        assert_eq!(reply, b"PONG abc\r\n");
    }

    #[test]
    fn tick_advances_the_clock_and_reports_the_next_deadline() {
        let mut client = new_client();
        client.handle_connected();
        while client.poll_transmit().is_some() {}
        let first_deadline = client
            .poll_timeout()
            .expect("keepalive is armed on connect");
        client.tick(first_deadline, 0.0);
        let next_deadline = client
            .poll_timeout()
            .expect("a fired deadline is replaced by the next one");
        assert!(
            next_deadline > first_deadline,
            "the dead-link timer now owns the next wake-up"
        );
    }

    #[test]
    fn command_forwards_to_the_engine() {
        let mut client = new_client();
        client.apply(Command::SetNick {
            nick: "other".to_string(),
        });
        let sent = client.poll_transmit().expect("a NICK command is sent");
        assert_eq!(sent, b"NICK other\r\n");
    }

    #[test]
    fn drain_events_collects_every_pending_event_in_one_call() {
        let mut client = new_client();
        client.handle_bytes(b":s 001 me :Welcome\r\n");
        client.handle_bytes(b":s 005 me CASEMAPPING=ascii :are supported\r\n");
        let events = client.drain_events();
        assert_eq!(events.len(), 2, "one call drains everything queued so far");
        assert!(
            client.drain_events().is_empty(),
            "a second call finds nothing left"
        );
    }

    #[test]
    fn command_json_matches_the_shape_a_js_host_sends() {
        let command: Command =
            serde_json::from_str(r##"{"type":"join","channel":"#obby","key":null}"##)
                .expect("a command object with an explicit null key parses");
        assert_eq!(
            command,
            Command::Join {
                channel: "#obby".to_string(),
                key: None,
            }
        );
    }

    #[test]
    fn command_json_treats_a_missing_optional_field_as_none() {
        let command: Command = serde_json::from_str(r#"{"type":"quit"}"#)
            .expect("an omitted optional field defaults to None");
        assert_eq!(command, Command::Quit { reason: None });
    }

    #[test]
    fn config_json_matches_the_shape_a_js_host_sends() {
        let config: Config = serde_json::from_str(
            r#"{"nick":"me","username":"me","realname":"me","password":null,"sasl":null,"retention":200,"alt_nicks":[]}"#,
        )
        .expect("every field Config declares is present");
        assert_eq!(config.nick, "me");
    }

    #[test]
    fn a_javascript_host_only_has_to_supply_the_nick() {
        let config: Config = serde_json::from_str(r#"{"nick":"me"}"#)
            .expect("Config carries serde defaults for everything but the nick");
        assert_eq!(config.nick, "me");
        assert!(config.alt_nicks.is_empty());
        assert!(config.sasl.is_none());
    }
}
