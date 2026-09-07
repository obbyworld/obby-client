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

use ::obby_client::{Client as CoreClient, Now};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

mod convert;

/// One connection, wrapped for Python.
///
/// No method here can panic: a bad argument becomes a `ValueError` with a message, because a panic
/// that unwinds into CPython aborts the interpreter instead of raising an exception.
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
    fn connected(&mut self, py: Python<'_>) {
        let inner = &mut self.inner;
        py.detach(move || inner.handle_connected());
    }

    /// Tell the engine its transport died. The model survives, so a reconnect can resume from it.
    fn disconnected(&mut self, py: Python<'_>) {
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
