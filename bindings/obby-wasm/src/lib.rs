//! WebAssembly bindings for `obby-client`.
//!
//! One `wasm-pack build --target web` artifact serves both the browser and Bun, so nothing here
//! may assume a DOM, a `window`, or a Node built-in. [`ObbyClient`] wraps [`Client`] and mirrors its poll/drain shape: a [`Command`] goes
//! in as a JS value, every pending [`Event`] comes out at once as a JS array, and bytes come out
//! separately from events, matching `poll_transmit` versus `poll_event` in the wrapped API.

use obby_client::{Client, Command, Config, Event, Now};
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

/// One connection, wrapped for JavaScript.
///
/// Every method mirrors one on [`Client`]. None of them can panic: a bad argument comes back as a
/// rejected `Result`, which `wasm-bindgen` turns into a thrown JS error, because a panic inside a
/// WebAssembly module poisons it for the rest of the host's lifetime, with no way to recover.
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
    pub fn tick(&mut self, monotonic_ms: u64, unix_ms: u64) {
        self.inner.tick(Now {
            monotonic_ms,
            unix_ms,
        });
    }

    /// When [`Self::tick`] next has something to do, as a monotonic instant, or `undefined` when
    /// nothing is scheduled. A host can set one timer for exactly this instant instead of polling
    /// on an interval.
    #[wasm_bindgen(js_name = pollTimeout)]
    pub fn poll_timeout(&self) -> Option<u64> {
        self.inner.poll_timeout()
    }

    /// Do something on this connection. `command` is a JS object with the same shape as
    /// [`Command`].
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
        client.tick(first_deadline, 0);
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
        client.apply(Command::Nick {
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
