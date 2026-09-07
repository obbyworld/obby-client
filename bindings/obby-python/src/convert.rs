//! JSON conversions that never touch the Python interpreter.
//!
//! Keeping these pure Rust functions separate from `lib.rs` is what makes them testable with
//! `cargo test`: this crate builds with `pyo3`'s `extension-module` feature, which does not link
//! against `libpython`, so a `cargo test` binary built from it cannot attach to a Python
//! interpreter at all. Anything that needs one, constructing a `PyObject`, importing `json` from
//! CPython, lives in `lib.rs` instead, exercised only through a real interpreter via `maturin
//! develop` plus a Python smoke test.

use ::obby_client::{Client, Command, Config, Event, Signal};

/// Parse a config sent as JSON text, in the shape `Config`'s serde derive expects.
pub(crate) fn config_from_json(json: &str) -> Result<Config, serde_json::Error> {
    serde_json::from_str(json)
}

/// Parse a command sent as JSON text, in the shape `Command`'s serde derive expects.
pub(crate) fn command_from_json(json: &str) -> Result<Command, serde_json::Error> {
    serde_json::from_str(json)
}

/// Parse a voice signalling frame sent as JSON text, in the shape `Signal`'s serde derive expects.
pub(crate) fn signal_from_json(json: &str) -> Result<Signal, serde_json::Error> {
    serde_json::from_str(json)
}

/// Every event queued since the last drain, oldest first.
///
/// One call collects the whole batch rather than one call per event, which is what keeps this
/// binding cheap across the Python GIL: a call into a `#[pymethods]` function costs the same
/// whether it carries one event or a hundred, so paying that cost once per drain rather than once
/// per event is what actually saves work.
pub(crate) fn drain_events(client: &mut Client) -> Vec<Event> {
    let mut events = Vec::new();
    while let Some(event) = client.poll_event() {
        events.push(event);
    }
    events
}

/// Encode a batch of events as one JSON array, the shape handed to Python's `json.loads`.
pub(crate) fn events_to_json(events: &[Event]) -> Result<String, serde_json::Error> {
    serde_json::to_string(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_json_needs_only_a_nick() {
        let config = config_from_json(r#"{"nick":"me"}"#).expect("nick is the only required field");
        assert_eq!(config.nick, "me");
        assert!(config.alt_nicks.is_empty());
        assert!(config.sasl.is_none());
    }

    #[test]
    fn config_from_json_rejects_invalid_json() {
        assert!(config_from_json("not json").is_err());
    }

    #[test]
    fn command_from_json_parses_a_join() {
        let command = command_from_json(r##"{"type":"join","channel":"#obby","key":null}"##)
            .expect("a well-formed command parses");
        assert_eq!(
            command,
            Command::Join {
                channel: "#obby".to_string(),
                key: None,
            }
        );
    }

    #[test]
    fn command_from_json_treats_a_missing_optional_field_as_none() {
        let command = command_from_json(r#"{"type":"quit"}"#)
            .expect("an omitted optional field defaults to None");
        assert_eq!(command, Command::Quit { reason: None });
    }

    #[test]
    fn command_from_json_rejects_invalid_json() {
        assert!(command_from_json("not json").is_err());
    }

    #[test]
    fn command_from_json_rejects_an_object_without_a_command_tag() {
        assert!(command_from_json("{}").is_err());
    }

    #[test]
    fn signal_from_json_parses_a_join() {
        let signal = signal_from_json(r#"{"type":"join","channel":"^general"}"#)
            .expect("a well-formed frame parses");
        assert_eq!(
            signal,
            Signal::Join {
                channel: "^general".to_string(),
            }
        );
    }

    #[test]
    fn signal_from_json_rejects_a_frame_type_that_does_not_exist() {
        assert!(signal_from_json(r#"{"type":"not_a_real_frame"}"#).is_err());
    }

    #[test]
    fn drain_events_collects_everything_queued_so_far() {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s 001 me :Welcome\r\n");
        client.handle_bytes(b":s 005 me CASEMAPPING=ascii :are supported\r\n");
        let events = drain_events(&mut client);
        assert_eq!(events.len(), 2, "one call drains everything queued so far");
        assert!(
            drain_events(&mut client).is_empty(),
            "a second call finds nothing left"
        );
    }

    #[test]
    fn events_to_json_produces_an_array() {
        let mut client = Client::new(Config::new("me"));
        client.handle_bytes(b":s 001 me :Welcome\r\n");
        let json = events_to_json(&drain_events(&mut client)).expect("events serialise");
        assert!(json.starts_with('[') && json.ends_with(']'));
        assert!(json.contains("registered"));
    }
}
