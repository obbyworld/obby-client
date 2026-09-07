//! Python bindings for `obby-client`, packaged with maturin.
//!
//! [`Client`] wraps [`obby_client::Client`] and mirrors its poll/drain shape, the same one
//! `bindings/obby-wasm` binds into JavaScript: a command goes in, every pending event comes out at
//! once as a batch, and bytes come out separately from events, matching `poll_transmit` versus
//! `poll_event` in the wrapped API.
//!
//! `pythonize` is not part of this build. A command and the config both cross as JSON text rather
//! than a native `dict`: without `pythonize`, the only other way to turn an arbitrary Python
//! object into a `serde_json::Value` is a hand-written recursive walk of it, which is exactly the
//! class of code `pythonize` exists to replace, and which is expensive to get right under the
//! "never panic" rule (nested containers, non-string keys, an int that does not fit an `i64`). A
//! JSON string sidesteps all of that: `serde_json::from_str` either parses or turns into a
//! `ValueError`, and it is also what lets the parser be exercised by `cargo test` with no
//! interpreter attached. Building one in Python is one `json.dumps(...)` call away.
//!
//! Events and the model cross the other direction, from an owned Rust value out to Python, where
//! the natural shape is a real `dict`/`list` rather than a string the caller has to parse again.
//! The standard library's `json.loads` builds that from the JSON text `serde_json` already
//! produces, which is far less code than a hand-written `serde_json::Value` to `PyObject` walker
//! and just as correct.

use ::obby_client::{Client as CoreClient, Command, Now, Typing};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

mod convert;

/// One connection, wrapped for Python.
///
/// No method here can panic: a bad argument becomes a `ValueError` with a message, because a panic
/// that unwinds into CPython aborts the interpreter instead of raising an exception.
///
/// ```python
/// import socket, time
/// from obby_client import Client
///
/// sock = socket.create_connection(("irc.example.org", 6667))
/// started = time.monotonic()
/// client = Client({"nick": "mynick"})
/// client.handle_connected()
///
/// while True:
///     while (out := client.poll_transmit()) is not None:
///         sock.sendall(out)
///
///     client.handle_bytes(sock.recv(4096))
///     client.tick(int((time.monotonic() - started) * 1000), int(time.time() * 1000))
///
///     for event in client.poll_events():
///         if event["type"] == "registered":
///             client.join("#obby")
///             client.send_message("#obby", "hello")
/// ```
#[pyclass(module = "obby_client")]
pub struct Client {
    inner: CoreClient,
}

#[pymethods]
impl Client {
    /// Build an engine that has not connected yet. Nothing is written until [`Self::handle_connected`].
    ///
    /// `config` is a dict, or JSON text, with the same shape as `obby_client::Config`. Only `nick`
    /// is required; every other field has a default.
    #[new]
    #[pyo3(text_signature = "(config)")]
    fn new(config: &Bound<'_, PyAny>) -> PyResult<Self> {
        let config = python_to_json(config)?;
        let config = convert::config_from_json(&config)
            .map_err(|err| PyValueError::new_err(format!("invalid config: {err}")))?;
        Ok(Self {
            inner: CoreClient::new(config),
        })
    }

    /// Tell the engine the transport is up. Queues the registration burst.
    fn handle_connected(&mut self, py: Python<'_>) {
        let inner = &mut self.inner;
        py.detach(move || inner.handle_connected());
    }

    /// Tell the engine its transport died. The model survives, so a reconnect can resume from it.
    fn handle_disconnected(&mut self, py: Python<'_>) {
        let inner = &mut self.inner;
        py.detach(move || inner.handle_disconnected());
    }

    /// Advance the clock. `monotonic_ms` drives every deadline; `unix_ms` only stamps a message
    /// the server did not stamp itself with `server-time`.
    #[pyo3(text_signature = "(monotonic_ms, unix_ms)")]
    fn tick(&mut self, py: Python<'_>, monotonic_ms: u64, unix_ms: u64) {
        let inner = &mut self.inner;
        py.detach(move || {
            inner.tick(Now {
                monotonic_ms,
                unix_ms,
            });
        });
    }

    /// When [`Self::tick`] next has something to do, as a monotonic instant, or `None` when
    /// nothing is scheduled. A host can set one timer for exactly this instant instead of polling
    /// on an interval.
    fn poll_timeout(&self) -> Option<u64> {
        self.inner.poll_timeout()
    }

    /// Do something on this connection. `command` is JSON text with the same shape as
    /// `obby_client::Command`.
    #[pyo3(text_signature = "(command)")]
    fn command(&mut self, command: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = command.py();
        let command = python_to_json(command)?;
        let command = convert::command_from_json(&command)
            .map_err(|err| PyValueError::new_err(format!("invalid command: {err}")))?;
        let inner = &mut self.inner;
        py.detach(move || inner.command(command));
        Ok(())
    }

    /// Join a channel.
    ///
    /// ```python
    /// client.join("#obby")
    /// client.join("#staff", key="hunter2")
    /// ```
    #[pyo3(signature = (channel, key=None))]
    fn join(&mut self, channel: String, key: Option<String>) {
        self.inner.command(Command::Join { channel, key });
    }

    /// Leave a channel.
    #[pyo3(signature = (channel, reason=None))]
    fn part(&mut self, channel: String, reason: Option<String>) {
        self.inner.command(Command::Part { channel, reason });
    }

    /// Say something to a channel or a person.
    ///
    /// ```python
    /// client.send_message("#obby", "hello there")
    /// client.send_message("alice", "a private word")
    /// ```
    fn send_message(&mut self, target: String, text: String) {
        self.inner.command(Command::SendMessage { target, text });
    }

    /// Send a notice, which by convention must never be auto-replied to.
    fn send_notice(&mut self, target: String, text: String) {
        self.inner.command(Command::SendNotice { target, text });
    }

    /// Send a `CTCP ACTION`, the third-person form.
    fn send_action(&mut self, target: String, text: String) {
        self.inner.command(Command::SendAction { target, text });
    }

    /// Change our nick.
    fn set_nick(&mut self, nick: String) {
        self.inner.command(Command::SetNick { nick });
    }

    /// Set or clear a channel topic.
    #[pyo3(signature = (channel, topic=None))]
    fn set_topic(&mut self, channel: String, topic: Option<String>) {
        self.inner.command(Command::SetTopic { channel, topic });
    }

    /// Mark ourselves away, or come back.
    #[pyo3(signature = (message=None))]
    fn set_away(&mut self, message: Option<String>) {
        self.inner.command(Command::SetAway { message });
    }

    /// Say we are typing, so others can show it. `state` is one of `"active"`, `"paused"`,
    /// `"done"`.
    fn set_typing(&mut self, target: String, state: &str) -> PyResult<()> {
        let state = match state {
            "active" => Typing::Active,
            "paused" => Typing::Paused,
            "done" => Typing::Done,
            other => {
                return Err(PyValueError::new_err(format!(
                    "invalid typing state: {other:?}, expected active, paused or done"
                )));
            }
        };
        self.inner.command(Command::SetTyping { target, state });
        Ok(())
    }

    /// React to a message with an emoji.
    fn add_reaction(&mut self, target: String, msgid: String, emoji: String) {
        self.inner.command(Command::AddReaction {
            target,
            msgid,
            emoji,
        });
    }

    /// Take a reaction back.
    fn remove_reaction(&mut self, target: String, msgid: String, emoji: String) {
        self.inner.command(Command::RemoveReaction {
            target,
            msgid,
            emoji,
        });
    }

    /// Ask the server to delete a message.
    #[pyo3(signature = (target, msgid, reason=None))]
    fn redact_message(&mut self, target: String, msgid: String, reason: Option<String>) {
        self.inner.command(Command::RedactMessage {
            target,
            msgid,
            reason,
        });
    }

    /// Tell the server how far we have read, in milliseconds since the Unix epoch.
    fn mark_read(&mut self, target: String, at_ms: u64) {
        self.inner.command(Command::MarkRead { target, at_ms });
    }

    /// Ask for older messages than the ones we hold. With no `before_msgid`, this asks for the
    /// most recent, which is what a fresh window wants.
    #[pyo3(signature = (target, before_msgid=None, limit=50))]
    fn fetch_history(&mut self, target: String, before_msgid: Option<String>, limit: u16) {
        self.inner.command(Command::FetchHistory {
            target,
            before_msgid,
            limit,
        });
    }

    /// Set one of our own metadata keys, or clear it.
    #[pyo3(signature = (key, value=None))]
    fn set_metadata(&mut self, key: String, value: Option<String>) {
        self.inner.command(Command::SetMetadata { key, value });
    }

    /// Ask to be told when these metadata keys change on anyone we can see.
    fn subscribe_metadata(&mut self, keys: Vec<String>) {
        self.inner.command(Command::SubscribeMetadata { keys });
    }

    /// Watch these nicks, so the server says when they come and go.
    fn watch_nicks(&mut self, nicks: Vec<String>) {
        self.inner.command(Command::WatchNicks { nicks });
    }

    /// Stop watching these nicks.
    fn unwatch_nicks(&mut self, nicks: Vec<String>) {
        self.inner.command(Command::UnwatchNicks { nicks });
    }

    /// Send a voice signalling frame to a room. The frame is the host's to build: everything in
    /// it comes from the media stack the core deliberately knows nothing about.
    fn send_voice_signal(&mut self, channel: String, signal: &Bound<'_, PyAny>) -> PyResult<()> {
        let signal = convert::signal_from_json(&python_to_json(signal)?)
            .map_err(|err| PyValueError::new_err(format!("invalid voice signal: {err}")))?;
        self.inner
            .command(Command::SendVoiceSignal { channel, signal });
        Ok(())
    }

    /// Leave the network.
    #[pyo3(signature = (reason=None))]
    fn quit(&mut self, reason: Option<String>) {
        self.inner.command(Command::Quit { reason });
    }

    /// Send a line we do not model. The escape hatch, so a host is never stuck waiting for us.
    fn send_raw_line(&mut self, line: String) {
        self.inner.command(Command::SendRawLine { line });
    }

    /// Feed whatever the transport read. Partial lines are held until the rest arrives.
    #[pyo3(text_signature = "(data)")]
    fn handle_bytes(&mut self, py: Python<'_>, data: Vec<u8>) {
        let inner = &mut self.inner;
        py.detach(move || inner.handle_bytes(&data));
    }

    /// Bytes the host should write to the transport, or `None` when there are none.
    ///
    /// One chunk per call, unlike [`Self::poll_events`]: a chunk is already the smallest unit a
    /// socket writes, so there is nothing to gain from batching it.
    fn poll_transmit(&mut self) -> Option<Vec<u8>> {
        self.inner.poll_transmit()
    }

    /// Every event the engine has queued since the last call, as a Python list.
    ///
    /// Draining a batch instead of one event per call is what keeps this binding cheap: a call
    /// across the GIL costs the same whether it carries one event or a hundred, so paying that
    /// cost once per drain rather than once per event is what actually saves work.
    ///
    /// An event's `type` names it, in `snake_case`, and the rest of the dict is that event's
    /// fields.
    ///
    /// ```python
    /// for event in client.poll_events():
    ///     if event["type"] == "registered":
    ///         print("registered as", event["nick"])
    ///     elif event["type"] == "model_changed":
    ///         print(event["change"])
    ///     elif event["type"] == "server_reply":
    ///         print(event["severity"], event["code"], event["text"])
    /// ```
    fn poll_events(&mut self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let inner = &mut self.inner;
        let json = py
            .detach(move || convert::events_to_json(&convert::drain_events(inner)))
            .map_err(|err| PyValueError::new_err(format!("could not encode events: {err}")))?;
        json_to_python(py, &json)
    }

    /// Everything the connection knows: channels, members, conversations and messages. For a host
    /// that only wants the model, not a diff of what changed.
    #[getter]
    fn model(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let inner = &self.inner;
        let json = py
            .detach(move || serde_json::to_string(inner.model()))
            .map_err(|err| PyValueError::new_err(format!("could not encode the model: {err}")))?;
        json_to_python(py, &json)
    }
}

/// Parse JSON text into a native Python object, through the standard library's own decoder.
///
/// Going through `json.loads` here keeps this file free of a hand-written `serde_json::Value` to
/// `PyObject` walker: CPython's decoder already exists, is implemented in C, and agrees with
/// `serde_json` on what JSON is.
fn json_to_python(py: Python<'_>, json: &str) -> PyResult<Py<PyAny>> {
    PyModule::import(py, "json")?
        .call_method1("loads", (json,))
        .map(Bound::unbind)
}

/// Turn a Python value into JSON text.
///
/// A string is taken as JSON already, so a caller may pass either a dict or the encoded form.
/// Anything else goes through `json.dumps`, for the same reason the decoder does: CPython's is in C
/// and already agrees with `serde_json` on what JSON is.
fn python_to_json(value: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(text) = value.extract::<String>() {
        return Ok(text);
    }
    PyModule::import(value.py(), "json")?
        .call_method1("dumps", (value,))?
        .extract()
}

/// The extension module Python imports as `obby_client`.
#[pymodule]
fn obby_client(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Client>()?;
    Ok(())
}
