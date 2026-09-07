//! The C ABI for obby-client.
//!
//! # Thread safety
//!
//! A handle is not synchronised. Every function here takes exclusive use of it for the duration of
//! the call, so a caller must never use one handle from two threads at once, even for two calls
//! that only read. Doing so is undefined behaviour rather than a race that merely produces a wrong
//! answer, because two exclusive references to the same engine exist at the same moment.
//!
//! Handles are independent of each other. A caller wanting several connections in parallel gives
//! each thread its own handle; a caller wanting one connection shared between threads puts its own
//! lock around the handle, because this library has no way to.
//!
//! One opaque handle, [`ObbyClient`], created by [`obby_client_new`] and destroyed by
//! [`obby_client_free`]. Everything else mirrors the four inputs and three outputs of
//! [`obby_client::Client`]: bytes and time go in, bytes and events come out.
//!
//! A C caller works in C types. [`obby_client_new`] takes an [`ObbyConfig`] struct, the commands a
//! client sends every day are one function each ([`obby_client_join`], [`obby_client_send_message`]
//! and the rest), and an event arrives as an [`ObbyEvent`] whose kind and fields are read with
//! [`obby_event_get_kind`] and [`obby_event_text`]. Nothing on that path asks a C program to build or
//! parse JSON.
//!
//! JSON remains the escape hatch for the long tail, since the alternative is one exported function
//! per command variant and one struct per event shape, both of which change whenever the protocol
//! does. [`obby_client_command_from_json`] submits any [`Command`],
//! [`obby_client_new_from_json`] takes a whole [`Config`] including SASL credentials,
//! [`obby_event_json`] gives an event's full body, and [`obby_client_poll_events_json`] drains every
//! pending event at once, which is what a binding for a language with a JSON parser wants.
//!
//! This crate is the one place `unsafe` lives in the workspace. Every function that takes a raw
//! pointer documents exactly what the caller must guarantee, and none of them panics: a null
//! pointer, a bad length, invalid UTF-8 or malformed JSON all return an error code or a null result
//! instead.

use std::ffi::{CStr, CString, c_char};
use std::ptr;
use std::sync::OnceLock;

use obby_client::{Client, Command, Config, Event, Now};

/// One connection's engine state.
///
/// Created by [`obby_client_new`] and destroyed by [`obby_client_free`]. The caller only ever holds
/// a pointer to one; every operation on it goes through a function in this crate.
pub struct ObbyClient(Client);

/// An owned byte buffer handed out by [`obby_client_poll_transmit`].
///
/// Freed with [`obby_client_free_bytes`], the only legal way to release it. An empty buffer is
/// always the zeroed value: a null `ptr` and a zero `len`, which is safe to free as a no-op.
#[repr(C)]
pub struct ObbyBytes {
    /// The first byte, or null when `len` is zero.
    pub ptr: *mut u8,
    /// How many bytes `ptr` points to.
    pub len: usize,
}

impl ObbyBytes {
    const fn empty() -> Self {
        Self {
            ptr: ptr::null_mut(),
            len: 0,
        }
    }
}

/// Turn a raw client pointer into a reference, treating null as absent rather than a fault.
///
/// # Safety
/// `client` must be null or a pointer returned by [`obby_client_new`] or [`obby_client_new_from_json`] that has not
/// since been passed to [`obby_client_free`].
/// Borrow the engine behind a handle.
///
/// The returned reference is exclusive and its lifetime is unconstrained, so the caller must have
/// exclusive use of the handle for as long as it lives. That is what makes concurrent calls on one
/// handle undefined behaviour, and it is why every exported function documents the rule.
///
/// The handle must not be in use on another thread while this call runs.
unsafe fn as_client<'a>(client: *mut ObbyClient) -> Option<&'a mut Client> {
    if client.is_null() {
        return None;
    }
    Some(unsafe { &mut (*client).0 })
}

/// Read a NUL-terminated C string as UTF-8, treating null and invalid UTF-8 as absent.
///
/// # Safety
/// `ptr` must be null or point to a NUL-terminated byte sequence valid for reads for the duration
/// of the borrow.
unsafe fn str_from_ptr<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

/// Build a byte buffer for the caller, freed with [`obby_client_free_bytes`].
///
/// Boxing the slice first guarantees the allocation's capacity equals its length, which is what
/// lets the free side reconstruct it from just a pointer and a length.
fn bytes_out(data: Vec<u8>) -> ObbyBytes {
    if data.is_empty() {
        return ObbyBytes::empty();
    }
    let boxed = data.into_boxed_slice();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed).cast::<u8>();
    ObbyBytes { ptr, len }
}

/// Build a NUL-terminated string for the caller, freed with [`obby_client_free_string`].
///
/// Null stands for "could not be produced" rather than panicking; the only way this can happen here
/// is a value that embeds a raw NUL byte, which valid JSON output never does.
fn string_out(s: String) -> *mut c_char {
    CString::new(s).map_or_else(|_| ptr::null_mut(), CString::into_raw)
}

/// Create a client from a JSON-encoded [`Config`], for a caller that wants a field
/// [`ObbyConfig`] does not have, such as SASL credentials or alternate nicks.
///
/// Returns null when `config_json` is null, is not valid UTF-8, or does not parse as a `Config`.
///
/// # Safety
/// `config_json` must be null or point to a NUL-terminated, valid UTF-8 C string, valid for reads
/// for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_new_from_json(config_json: *const c_char) -> *mut ObbyClient {
    let Some(json) = (unsafe { str_from_ptr(config_json) }) else {
        return ptr::null_mut();
    };
    let Ok(config) = serde_json::from_str::<Config>(json) else {
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(ObbyClient(Client::new(config))))
}

/// Destroy a client created by [`obby_client_new`].
///
/// # Safety
/// `client` must be null or a pointer returned by [`obby_client_new`] that has not already been
/// freed. It must not be used again after this call.///
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_free(client: *mut ObbyClient) {
    if client.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(client) });
}

/// Tell the engine the transport is up. See [`obby_client::Client::handle_connected`].
///
/// # Safety
/// `client` must be null or a valid, non-freed pointer from [`obby_client_new`].
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_handle_connected(client: *mut ObbyClient) {
    if let Some(client) = unsafe { as_client(client) } {
        client.handle_connected();
    }
}

/// Tell the engine its transport died. See [`obby_client::Client::handle_disconnected`].
///
/// # Safety
/// `client` must be null or a valid, non-freed pointer from [`obby_client_new`].
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_handle_disconnected(client: *mut ObbyClient) {
    if let Some(client) = unsafe { as_client(client) } {
        client.handle_disconnected();
    }
}

/// Feed bytes read from the transport.
///
/// A null `data` is only valid when `len` is zero; any other null-with-nonzero-length call is
/// treated as if nothing were fed, rather than read out of bounds.
///
/// # Safety
/// `client` must be null or valid. When `len` is greater than zero, `data` must point to at least
/// `len` readable bytes, valid for the duration of this call.
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_handle_bytes(
    client: *mut ObbyClient,
    data: *const u8,
    len: usize,
) {
    let Some(client) = (unsafe { as_client(client) }) else {
        return;
    };
    if len == 0 {
        client.handle_bytes(&[]);
        return;
    }
    if data.is_null() {
        return;
    }
    client.handle_bytes(unsafe { std::slice::from_raw_parts(data, len) });
}

/// Drain one pending outbound buffer, or the empty [`ObbyBytes`] when there is none.
///
/// # Safety
/// `client` must be null or a valid, non-freed pointer from [`obby_client_new`].
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_poll_transmit(client: *mut ObbyClient) -> ObbyBytes {
    unsafe { as_client(client) }
        .and_then(Client::poll_transmit)
        .map_or_else(ObbyBytes::empty, bytes_out)
}

/// Free a buffer returned by [`obby_client_poll_transmit`].
///
/// The only legal way to release one. Freeing the zeroed value (null `ptr`, zero `len`) is a no-op.
///
/// # Safety
/// `bytes` must be a value returned by [`obby_client_poll_transmit`] that has not already been
/// freed, or the zeroed value.///
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_free_bytes(bytes: ObbyBytes) {
    if bytes.ptr.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(bytes.ptr, bytes.len)) });
}

/// Drain every pending event as one JSON array, `"[]"` when there are none.
///
/// Returns null only when `client` is null; a working client always produces valid JSON.
///
/// # Safety
/// `client` must be null or a valid, non-freed pointer from [`obby_client_new`].
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_poll_events_json(client: *mut ObbyClient) -> *mut c_char {
    let Some(client) = (unsafe { as_client(client) }) else {
        return ptr::null_mut();
    };
    let mut events = Vec::new();
    while let Some(event) = client.poll_event() {
        events.push(event);
    }
    serde_json::to_string(&events).map_or_else(|_| ptr::null_mut(), string_out)
}

/// Free a string returned by [`obby_client_poll_events_json`] or [`obby_client_model_json`].
///
/// The only legal way to release one. Never pass the pointer returned by [`obby_client_version`]
/// here: that one is static and owned by the library, not by the caller.
///
/// # Safety
/// `s` must be null, or a pointer returned by [`obby_client_poll_events_json`] or
/// [`obby_client_model_json`] that has not already been freed.///
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_free_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    drop(unsafe { CString::from_raw(s) });
}

/// Advance the clock. See [`obby_client::Client::tick`].
///
/// # Safety
/// `client` must be null or a valid, non-freed pointer from [`obby_client_new`].
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_tick(
    client: *mut ObbyClient,
    monotonic_ms: u64,
    unix_ms: u64,
) {
    if let Some(client) = unsafe { as_client(client) } {
        client.tick(Now {
            monotonic_ms,
            unix_ms,
        });
    }
}

/// When the engine next has something to do, if ever.
///
/// Returns `true` and writes the deadline to `*out_ms` when one is pending; returns `false`
/// otherwise, writing zero to `*out_ms` when it is non-null so the caller never reads uninitialised
/// memory either way.
///
/// # Safety
/// `client` must be null or valid. `out_ms` must be null or point to a writable `u64` valid for the
/// duration of this call.///
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_poll_timeout(
    client: *mut ObbyClient,
    out_ms: *mut u64,
) -> bool {
    let timeout = unsafe { as_client(client) }.and_then(|client| client.poll_timeout());
    if let Some(out_ms) = unsafe { out_ms.as_mut() } {
        *out_ms = timeout.unwrap_or(0);
    }
    timeout.is_some()
}

/// Submit any command, as JSON.
///
/// Every command has this form; the ones a client sends constantly also have a function of their
/// own, such as [`obby_client_join`].
///
/// Returns `true` when it parsed and was submitted, `false` when `client` or `command_json` is
/// null, `command_json` is not valid UTF-8, or it does not parse as a `Command`.
///
/// # Safety
/// `client` must be null or valid. `command_json` must be null or point to a NUL-terminated, valid
/// UTF-8 C string, valid for reads for the duration of this call.
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_command_from_json(
    client: *mut ObbyClient,
    command_json: *const c_char,
) -> bool {
    let Some(client) = (unsafe { as_client(client) }) else {
        return false;
    };
    let Some(json) = (unsafe { str_from_ptr(command_json) }) else {
        return false;
    };
    let Ok(command) = serde_json::from_str::<Command>(json) else {
        return false;
    };
    client.command(command);
    true
}

/// Read the whole model as JSON, for a host that wants the full state rather than the changes.
///
/// Returns null only when `client` is null; a working client always produces valid JSON.
///
/// # Safety
/// `client` must be null or a valid, non-freed pointer from [`obby_client_new`].
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_model_json(client: *mut ObbyClient) -> *mut c_char {
    let Some(client) = (unsafe { as_client(client) }) else {
        return ptr::null_mut();
    };
    serde_json::to_string(client.model()).map_or_else(|_| ptr::null_mut(), string_out)
}

/// The crate version, as a static, NUL-terminated string.
///
/// This pointer is owned by the library and lives for the process's lifetime: never pass it to
/// [`obby_client_free_string`].
#[unsafe(no_mangle)]
pub extern "C" fn obby_client_version() -> *const c_char {
    static VERSION: OnceLock<CString> = OnceLock::new();
    VERSION
        .get_or_init(|| CString::new(env!("CARGO_PKG_VERSION")).unwrap_or_default())
        .as_ptr()
}

/// A client's settings, in C types.
///
/// Only `nick` is required. Every pointer may be null, and a null means "use the default": the
/// nick for `username` and `realname`, no password, no SASL. A zero `retention` keeps the engine's
/// own message limit.
///
/// [`obby_client_new_from_json`] takes the fields this omits, such as SASL credentials and the
/// alternate nicks to try when one is taken.
#[repr(C)]
pub struct ObbyConfig {
    /// The nick to register with. Required.
    pub nick: *const c_char,
    /// The username sent in `USER`, or null for the nick.
    pub username: *const c_char,
    /// The realname sent in `USER`, or null for the nick.
    pub realname: *const c_char,
    /// The server password sent as `PASS`, or null for none.
    pub password: *const c_char,
    /// How many messages each channel and conversation keeps, or 0 for the default.
    pub retention: usize,
}

/// What an [`ObbyEvent`] is.
///
/// `Unknown` covers an event this ABI has no case for yet, which a caller reads with
/// [`obby_event_json`] rather than being blind to it.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObbyEventKind {
    /// An event this ABI does not name. Read it with [`obby_event_json`].
    Unknown = 0,
    /// The server acknowledged the capabilities we asked for.
    CapabilitiesAcknowledged,
    /// Registration finished and the connection is usable.
    Registered,
    /// One `005` token, with its value when it has one.
    IsupportToken,
    /// SASL authentication succeeded.
    LoggedIn,
    /// SASL authentication failed.
    SaslFailed,
    /// The nick we asked for is taken.
    NickInUse,
    /// The model changed. The change itself is in [`obby_event_json`].
    ModelChanged,
    /// The link is dead and the host should redial.
    LinkDead,
    /// Redial after this many milliseconds.
    ReconnectAfter,
    /// Reconnection gave up.
    ReconnectAbandoned,
    /// A command we labelled went unanswered.
    CommandTimedOut,
    /// The commands the server lets us use changed.
    AllowedCommandsChanged,
    /// A voice signalling frame. The frame is in [`obby_event_json`].
    Voice,
    /// Someone started or stopped composing a message.
    TypingChanged,
    /// Someone we monitor came online or went offline.
    PresenceChanged,
    /// A `standard-replies` FAIL, WARN or NOTE.
    ServerReply,
    /// A line the engine does not model. The message is in [`obby_event_json`].
    RawLine,
}

/// One value on an [`ObbyEvent`].
///
/// An event only has the fields its kind defines, and [`obby_event_text`] answers null for the
/// rest. Numbers, including the booleans, are read with [`obby_event_number`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObbyEventField {
    /// The nick an event is about.
    Nick = 0,
    /// The account we authenticated as.
    Account,
    /// A `005` token name.
    Token,
    /// A `005` token's value.
    Value,
    /// The nick the server refused.
    Refused,
    /// The nick being tried instead.
    Trying,
    /// Why something failed.
    Reason,
    /// The command an event is about.
    Command,
    /// The channel or nick an event is about.
    Target,
    /// The channel an event is about.
    Channel,
    /// A numeric or named reply code.
    Code,
    /// Human-readable text from the server.
    Text,
    /// `fail`, `warn` or `note`.
    Severity,
    /// The capability names the server acknowledged, separated by spaces.
    Names,
    /// How long to wait before redialling, in milliseconds. Read with [`obby_event_number`].
    AfterMs,
    /// 1 when someone is composing, 0 when they stopped. Read with [`obby_event_number`].
    Active,
    /// 1 when someone is online, 0 when they are not. Read with [`obby_event_number`].
    Online,
}

/// One event, owned by the caller until [`obby_event_free`].
///
/// The strings [`obby_event_text`] and [`obby_event_json`] return point into this event and die
/// with it, so copy anything that must outlive the call to free.
pub struct ObbyEvent {
    kind: ObbyEventKind,
    text: Vec<(ObbyEventField, CString)>,
    numbers: Vec<(ObbyEventField, u64)>,
    json: CString,
}

impl ObbyEvent {
    fn text(&mut self, field: ObbyEventField, value: &str) {
        if let Ok(value) = CString::new(value) {
            self.text.push((field, value));
        }
    }

    fn number(&mut self, field: ObbyEventField, value: u64) {
        self.numbers.push((field, value));
    }
}

/// Flatten an event into the C view of it, keeping the whole thing as JSON alongside.
fn event_out(event: &Event) -> ObbyEvent {
    let json = serde_json::to_string(event).unwrap_or_default();
    let mut out = ObbyEvent {
        kind: ObbyEventKind::Unknown,
        text: Vec::new(),
        numbers: Vec::new(),
        json: CString::new(json).unwrap_or_default(),
    };
    match event {
        Event::CapabilitiesAcknowledged { names } => {
            out.kind = ObbyEventKind::CapabilitiesAcknowledged;
            out.text(ObbyEventField::Names, &names.join(" "));
        }
        Event::Registered { nick } => {
            out.kind = ObbyEventKind::Registered;
            out.text(ObbyEventField::Nick, nick);
        }
        Event::IsupportToken { token, value } => {
            out.kind = ObbyEventKind::IsupportToken;
            out.text(ObbyEventField::Token, token);
            if let Some(value) = value {
                out.text(ObbyEventField::Value, value);
            }
        }
        Event::LoggedIn { account } => {
            out.kind = ObbyEventKind::LoggedIn;
            out.text(ObbyEventField::Account, account);
        }
        Event::SaslFailed { reason } => {
            out.kind = ObbyEventKind::SaslFailed;
            out.text(ObbyEventField::Reason, &format!("{reason:?}"));
        }
        Event::NickInUse { refused, trying } => {
            out.kind = ObbyEventKind::NickInUse;
            out.text(ObbyEventField::Refused, refused);
            out.text(ObbyEventField::Trying, trying);
        }
        Event::ModelChanged { .. } => out.kind = ObbyEventKind::ModelChanged,
        Event::LinkDead => out.kind = ObbyEventKind::LinkDead,
        Event::ReconnectAfter { after_ms } => {
            out.kind = ObbyEventKind::ReconnectAfter;
            out.number(ObbyEventField::AfterMs, *after_ms);
        }
        Event::ReconnectAbandoned => out.kind = ObbyEventKind::ReconnectAbandoned,
        Event::CommandTimedOut { command } => {
            out.kind = ObbyEventKind::CommandTimedOut;
            out.text(ObbyEventField::Command, command);
        }
        #[cfg(feature = "obby")]
        Event::AllowedCommandsChanged => out.kind = ObbyEventKind::AllowedCommandsChanged,
        #[cfg(feature = "voice")]
        Event::Voice { channel, .. } => {
            out.kind = ObbyEventKind::Voice;
            out.text(ObbyEventField::Channel, channel);
        }
        Event::TypingChanged {
            target,
            nick,
            active,
        } => {
            out.kind = ObbyEventKind::TypingChanged;
            out.text(ObbyEventField::Target, target);
            out.text(ObbyEventField::Nick, nick);
            out.number(ObbyEventField::Active, u64::from(*active));
        }
        Event::PresenceChanged { nick, online } => {
            out.kind = ObbyEventKind::PresenceChanged;
            out.text(ObbyEventField::Nick, nick);
            out.number(ObbyEventField::Online, u64::from(*online));
        }
        Event::ServerReply {
            severity,
            command,
            code,
            text,
            ..
        } => {
            out.kind = ObbyEventKind::ServerReply;
            out.text(
                ObbyEventField::Severity,
                &format!("{severity:?}").to_lowercase(),
            );
            out.text(ObbyEventField::Command, command);
            out.text(ObbyEventField::Code, code);
            out.text(ObbyEventField::Text, text);
        }
        Event::RawLine { .. } => out.kind = ObbyEventKind::RawLine,
        // Event is non-exhaustive, so a version of the engine newer than this ABI reaches a C
        // caller as an unknown kind with its JSON intact rather than as a build failure
        _ => {}
    }
    out
}

/// Create a client.
///
/// Returns null when `config` is null, or when its `nick` is null or not valid UTF-8.
///
/// # Safety
/// `config` must be null or point to a readable [`ObbyConfig`] whose string fields are each null or
/// a NUL-terminated, valid UTF-8 C string, all valid for reads for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_new(config: *const ObbyConfig) -> *mut ObbyClient {
    let Some(config) = (unsafe { config.as_ref() }) else {
        return ptr::null_mut();
    };
    let Some(nick) = (unsafe { str_from_ptr(config.nick) }) else {
        return ptr::null_mut();
    };

    let mut settings = Config::new(nick);
    if let Some(username) = unsafe { str_from_ptr(config.username) } {
        username.clone_into(&mut settings.username);
    }
    if let Some(realname) = unsafe { str_from_ptr(config.realname) } {
        realname.clone_into(&mut settings.realname);
    }
    settings.password = unsafe { str_from_ptr(config.password) }.map(ToOwned::to_owned);
    if config.retention > 0 {
        settings.retention = config.retention;
    }

    Box::into_raw(Box::new(ObbyClient(Client::new(settings))))
}

/// Read `count` C strings out of an array, skipping any element that is null or not valid UTF-8.
///
/// # Safety
/// `ptr` must be null or point to `count` valid, NUL-terminated C strings, valid for reads for the
/// duration of this call.
unsafe fn strings_from_ptr_array(ptr: *const *const c_char, count: usize) -> Vec<String> {
    if ptr.is_null() {
        return Vec::new();
    }
    let elements = unsafe { std::slice::from_raw_parts(ptr, count) };
    elements
        .iter()
        .filter_map(|&s| unsafe { str_from_ptr(s) })
        .map(ToOwned::to_owned)
        .collect()
}

/// Submit a command built here rather than parsed from JSON.
///
/// Returns false when the client is null or a required string is null or not valid UTF-8.
unsafe fn submit(client: *mut ObbyClient, command: impl FnOnce() -> Option<Command>) -> bool {
    let Some(client) = (unsafe { as_client(client) }) else {
        return false;
    };
    let Some(command) = command() else {
        return false;
    };
    client.command(command);
    true
}

/// Join a channel, with `key` for a channel that needs one, or null.
///
/// # Safety
/// `client` must be null or valid. Each string must be null or a NUL-terminated, valid UTF-8 C
/// string, valid for reads for the duration of this call.
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_join(
    client: *mut ObbyClient,
    channel: *const c_char,
    key: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::Join {
                channel: str_from_ptr(channel)?.to_owned(),
                key: str_from_ptr(key).map(ToOwned::to_owned),
            })
        })
    }
}

/// Leave a channel, with `reason` shown to the others in it, or null.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_part(
    client: *mut ObbyClient,
    channel: *const c_char,
    reason: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::Part {
                channel: str_from_ptr(channel)?.to_owned(),
                reason: str_from_ptr(reason).map(ToOwned::to_owned),
            })
        })
    }
}

/// Say something to a channel or a person.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_send_message(
    client: *mut ObbyClient,
    target: *const c_char,
    text: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::SendMessage {
                target: str_from_ptr(target)?.to_owned(),
                text: str_from_ptr(text)?.to_owned(),
            })
        })
    }
}

/// Send a notice, which by convention must never be auto-replied to.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_send_notice(
    client: *mut ObbyClient,
    target: *const c_char,
    text: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::SendNotice {
                target: str_from_ptr(target)?.to_owned(),
                text: str_from_ptr(text)?.to_owned(),
            })
        })
    }
}

/// Send a `CTCP ACTION`, the third-person form.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_send_action(
    client: *mut ObbyClient,
    target: *const c_char,
    text: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::SendAction {
                target: str_from_ptr(target)?.to_owned(),
                text: str_from_ptr(text)?.to_owned(),
            })
        })
    }
}

/// Change our nick.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_set_nick(
    client: *mut ObbyClient,
    nick: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::SetNick {
                nick: str_from_ptr(nick)?.to_owned(),
            })
        })
    }
}

/// Set or clear a channel's topic. A null `topic` asks for the current one.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_set_topic(
    client: *mut ObbyClient,
    channel: *const c_char,
    topic: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::SetTopic {
                channel: str_from_ptr(channel)?.to_owned(),
                topic: str_from_ptr(topic).map(ToOwned::to_owned),
            })
        })
    }
}

/// Go away with a message, or come back by passing null.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_set_away(
    client: *mut ObbyClient,
    message: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::SetAway {
                message: str_from_ptr(message).map(ToOwned::to_owned),
            })
        })
    }
}

/// Quit, with a reason or null.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_quit(client: *mut ObbyClient, reason: *const c_char) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::Quit {
                reason: str_from_ptr(reason).map(ToOwned::to_owned),
            })
        })
    }
}

/// Say we are typing, so others can show it. `state` must be `"active"`, `"paused"` or `"done"`.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_set_typing(
    client: *mut ObbyClient,
    target: *const c_char,
    state: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            let state = match str_from_ptr(state)? {
                "active" => obby_client::Typing::Active,
                "paused" => obby_client::Typing::Paused,
                "done" => obby_client::Typing::Done,
                _ => return None,
            };
            Some(Command::SetTyping {
                target: str_from_ptr(target)?.to_owned(),
                state,
            })
        })
    }
}

/// React to a message with an emoji.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_add_reaction(
    client: *mut ObbyClient,
    target: *const c_char,
    msgid: *const c_char,
    emoji: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::AddReaction {
                target: str_from_ptr(target)?.to_owned(),
                msgid: str_from_ptr(msgid)?.to_owned(),
                emoji: str_from_ptr(emoji)?.to_owned(),
            })
        })
    }
}

/// Take a reaction back.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_remove_reaction(
    client: *mut ObbyClient,
    target: *const c_char,
    msgid: *const c_char,
    emoji: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::RemoveReaction {
                target: str_from_ptr(target)?.to_owned(),
                msgid: str_from_ptr(msgid)?.to_owned(),
                emoji: str_from_ptr(emoji)?.to_owned(),
            })
        })
    }
}

/// Ask the server to delete a message, with `reason` when it wants one, or null.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_redact_message(
    client: *mut ObbyClient,
    target: *const c_char,
    msgid: *const c_char,
    reason: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::RedactMessage {
                target: str_from_ptr(target)?.to_owned(),
                msgid: str_from_ptr(msgid)?.to_owned(),
                reason: str_from_ptr(reason).map(ToOwned::to_owned),
            })
        })
    }
}

/// Tell the server how far we have read, as the `server-time` of the last message read.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_mark_read(
    client: *mut ObbyClient,
    target: *const c_char,
    timestamp: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::MarkRead {
                target: str_from_ptr(target)?.to_owned(),
                timestamp: str_from_ptr(timestamp)?.to_owned(),
            })
        })
    }
}

/// Ask for older messages than the ones we hold. A null `before` asks for the most recent.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_fetch_history(
    client: *mut ObbyClient,
    target: *const c_char,
    before: *const c_char,
    limit: u16,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::FetchHistory {
                target: str_from_ptr(target)?.to_owned(),
                before: str_from_ptr(before).map(ToOwned::to_owned),
                limit,
            })
        })
    }
}

/// Set one of our own metadata keys, or clear it with a null `value`.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_set_metadata(
    client: *mut ObbyClient,
    key: *const c_char,
    value: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::SetMetadata {
                key: str_from_ptr(key)?.to_owned(),
                value: str_from_ptr(value).map(ToOwned::to_owned),
            })
        })
    }
}

/// Ask to be told when these metadata keys change on anyone we can see.
///
/// # Safety
/// As [`obby_client_join`]. `keys` must be null or point to `count` valid, NUL-terminated C
/// strings, valid for reads for the duration of this call; an element that is null or not valid
/// UTF-8 is skipped rather than failing the whole call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_subscribe_metadata(
    client: *mut ObbyClient,
    keys: *const *const c_char,
    count: usize,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::SubscribeMetadata {
                keys: strings_from_ptr_array(keys, count),
            })
        })
    }
}

/// Watch these nicks, so the server says when they come and go.
///
/// # Safety
/// As [`obby_client_subscribe_metadata`], with `nicks` in place of `keys`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_watch_nicks(
    client: *mut ObbyClient,
    nicks: *const *const c_char,
    count: usize,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::WatchNicks {
                nicks: strings_from_ptr_array(nicks, count),
            })
        })
    }
}

/// Stop watching these nicks.
///
/// # Safety
/// As [`obby_client_subscribe_metadata`], with `nicks` in place of `keys`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_unwatch_nicks(
    client: *mut ObbyClient,
    nicks: *const *const c_char,
    count: usize,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::UnwatchNicks {
                nicks: strings_from_ptr_array(nicks, count),
            })
        })
    }
}

/// Send a voice signalling frame to a room. `signal_json` is the frame, already encoded as the
/// JSON that travels in the tag.
///
/// # Safety
/// As [`obby_client_join`].
#[cfg(feature = "voice")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_send_voice_signal(
    client: *mut ObbyClient,
    channel: *const c_char,
    signal_json: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::SendVoiceSignal {
                channel: str_from_ptr(channel)?.to_owned(),
                signal_json: str_from_ptr(signal_json)?.to_owned(),
            })
        })
    }
}

/// Send one raw protocol line, without the trailing CRLF, for anything this ABI does not name.
///
/// # Safety
/// As [`obby_client_join`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_send_raw_line(
    client: *mut ObbyClient,
    line: *const c_char,
) -> bool {
    unsafe {
        submit(client, || {
            Some(Command::SendRawLine {
                line: str_from_ptr(line)?.to_owned(),
            })
        })
    }
}

/// Take the next event, or null when there are none.
///
/// The caller owns what comes back and releases it with [`obby_event_free`].
///
/// # Safety
/// `client` must be null or a valid, non-freed pointer from [`obby_client_new`].
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_poll_event(client: *mut ObbyClient) -> *mut ObbyEvent {
    let Some(client) = (unsafe { as_client(client) }) else {
        return ptr::null_mut();
    };
    let Some(event) = client.poll_event() else {
        return ptr::null_mut();
    };
    Box::into_raw(Box::new(event_out(&event)))
}

/// What kind of event this is.
///
/// A null event reads as [`ObbyEventKind::Unknown`], so a caller that skipped the null check gets a
/// kind it already has to handle rather than a crash.
///
/// # Safety
/// `event` must be null or a valid, non-freed pointer from [`obby_client_poll_event`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_event_get_kind(event: *const ObbyEvent) -> ObbyEventKind {
    unsafe { event.as_ref() }.map_or(ObbyEventKind::Unknown, |event| event.kind)
}

/// One of the event's string fields, or null when this event has no such field.
///
/// The pointer borrows from the event and is invalid once [`obby_event_free`] runs.
///
/// # Safety
/// `event` must be null or a valid, non-freed pointer from [`obby_client_poll_event`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_event_text(
    event: *const ObbyEvent,
    field: ObbyEventField,
) -> *const c_char {
    let Some(event) = (unsafe { event.as_ref() }) else {
        return ptr::null();
    };
    event
        .text
        .iter()
        .find(|(name, _)| *name == field)
        .map_or(ptr::null(), |(_, value)| value.as_ptr())
}

/// One of the event's numeric fields, written to `out`.
///
/// Returns false, and leaves `out` alone, when this event has no such field.
///
/// # Safety
/// `event` must be null or a valid, non-freed pointer from [`obby_client_poll_event`]. `out` must
/// be null or point to a writable `uint64_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_event_number(
    event: *const ObbyEvent,
    field: ObbyEventField,
    out: *mut u64,
) -> bool {
    let Some(event) = (unsafe { event.as_ref() }) else {
        return false;
    };
    let Some((_, value)) = event.numbers.iter().find(|(name, _)| *name == field) else {
        return false;
    };
    if let Some(out) = unsafe { out.as_mut() } {
        *out = *value;
    }
    true
}

/// The whole event as JSON, for the parts this ABI does not flatten into fields.
///
/// The pointer borrows from the event and is invalid once [`obby_event_free`] runs.
///
/// # Safety
/// `event` must be null or a valid, non-freed pointer from [`obby_client_poll_event`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_event_json(event: *const ObbyEvent) -> *const c_char {
    unsafe { event.as_ref() }.map_or(ptr::null(), |event| event.json.as_ptr())
}

/// Release an event.
///
/// # Safety
/// `event` must be null, or a pointer from [`obby_client_poll_event`] that has not already been
/// passed here. Every string read from it is invalid afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_event_free(event: *mut ObbyEvent) {
    if event.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(event) });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_json(nick: &str) -> CString {
        CString::new(format!(
            r#"{{"nick":"{nick}","username":"{nick}","realname":"{nick}","password":null,"sasl":null,"retention":200,"alt_nicks":[]}}"#
        ))
        .expect("no interior NUL in a config built from a plain nick")
    }

    /// Drain every pending outbound buffer into one string, freeing each as it goes.
    fn drain(client: *mut ObbyClient) -> String {
        let mut out = Vec::new();
        loop {
            let chunk = unsafe { obby_client_poll_transmit(client) };
            if chunk.ptr.is_null() {
                break;
            }
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(chunk.ptr, chunk.len) });
            unsafe { obby_client_free_bytes(chunk) };
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Drain every pending event as parsed JSON, freeing the string that carried it.
    fn events(client: *mut ObbyClient) -> serde_json::Value {
        let raw = unsafe { obby_client_poll_events_json(client) };
        assert!(!raw.is_null(), "a live client always produces JSON");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_string();
        unsafe { obby_client_free_string(raw) };
        serde_json::from_str(&text).expect("poll_events always emits a JSON array")
    }

    #[test]
    fn drives_the_whole_surface_through_the_c_api() {
        let config = config_json("tester");
        let client = unsafe { obby_client_new_from_json(config.as_ptr()) };
        assert!(!client.is_null());

        unsafe { obby_client_handle_connected(client) };
        let sent = drain(client);
        assert!(sent.contains("CAP LS 302"));
        assert!(sent.contains("NICK tester"));
        assert!(sent.contains("USER tester 0 * tester"));

        let cap_ls = c":s CAP * LS :\r\n";
        unsafe {
            obby_client_handle_bytes(client, cap_ls.to_bytes().as_ptr(), cap_ls.to_bytes().len());
        }
        assert_eq!(drain(client), "CAP END\r\n");

        let welcome = c":s 001 tester :Welcome\r\n";
        unsafe {
            obby_client_handle_bytes(
                client,
                welcome.to_bytes().as_ptr(),
                welcome.to_bytes().len(),
            );
        }

        let mut timeout = 0u64;
        assert!(unsafe { obby_client_poll_timeout(client, &raw mut timeout) });

        let seen = events(client);
        assert!(
            seen.as_array()
                .expect("poll_events always emits an array")
                .iter()
                .any(|event| event["type"] == "registered"),
            "registration must reach the host as an event: {seen}"
        );

        let join = CString::new(r##"{"type":"join","channel":"#test","key":null}"##).unwrap();
        assert!(unsafe { obby_client_command_from_json(client, join.as_ptr()) });
        assert_eq!(drain(client), "JOIN #test\r\n");

        let model_raw = unsafe { obby_client_model_json(client) };
        assert!(!model_raw.is_null());
        let model_text = unsafe { CStr::from_ptr(model_raw) }
            .to_str()
            .unwrap()
            .to_string();
        unsafe { obby_client_free_string(model_raw) };
        let model: serde_json::Value = serde_json::from_str(&model_text).unwrap();
        assert_eq!(model["me"]["nick"], "tester");

        unsafe { obby_client_handle_disconnected(client) };
        unsafe { obby_client_free(client) };
    }

    #[test]
    fn version_is_a_static_nul_terminated_string() {
        let version = obby_client_version();
        assert!(!version.is_null());
        let text = unsafe { CStr::from_ptr(version) }.to_str().unwrap();
        assert_eq!(text, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn every_function_survives_a_null_client() {
        unsafe {
            obby_client_free(ptr::null_mut());
            obby_client_handle_connected(ptr::null_mut());
            obby_client_handle_disconnected(ptr::null_mut());
            obby_client_handle_bytes(ptr::null_mut(), ptr::null(), 0);
            obby_client_handle_bytes(ptr::null_mut(), b"x".as_ptr(), 1);
            obby_client_tick(ptr::null_mut(), 0, 0);

            let bytes = obby_client_poll_transmit(ptr::null_mut());
            assert!(bytes.ptr.is_null());
            assert_eq!(bytes.len, 0);

            assert!(obby_client_poll_events_json(ptr::null_mut()).is_null());

            let mut out_ms = 42u64;
            assert!(!obby_client_poll_timeout(ptr::null_mut(), &raw mut out_ms));
            assert_eq!(
                out_ms, 0,
                "false is paired with a defined value, never garbage"
            );

            let ok = CString::new(r#"{"type":"quit","reason":null}"#).unwrap();
            assert!(!obby_client_command_from_json(ptr::null_mut(), ok.as_ptr()));

            assert!(obby_client_model_json(ptr::null_mut()).is_null());
        }
    }

    #[test]
    fn a_null_data_pointer_with_a_nonzero_length_is_ignored_not_dereferenced() {
        let config = config_json("nully");
        let client = unsafe { obby_client_new_from_json(config.as_ptr()) };
        unsafe {
            obby_client_handle_bytes(client, ptr::null(), 10);
            obby_client_handle_bytes(client, ptr::null(), 0);
            obby_client_free(client);
        }
    }

    #[test]
    fn a_null_out_ms_is_never_written_through() {
        let client = unsafe { obby_client_new_from_json(config_json("nully").as_ptr()) };
        unsafe {
            assert!(!obby_client_poll_timeout(client, ptr::null_mut()));
            obby_client_free(client);
        }
    }

    /// A config naming only a nick, which is all the typed constructor requires.
    fn config(nick: &CStr) -> ObbyConfig {
        ObbyConfig {
            nick: nick.as_ptr(),
            username: ptr::null(),
            realname: ptr::null(),
            password: ptr::null(),
            retention: 0,
        }
    }

    #[test]
    fn a_c_caller_never_has_to_touch_json() {
        let nick = c"typed";
        let settings = config(nick);
        let client = unsafe { obby_client_new(&raw const settings) };
        assert!(!client.is_null());

        unsafe { obby_client_handle_connected(client) };
        assert!(drain(client).contains("NICK typed"));

        let welcome = c":s 001 typed :Welcome\r\n";
        unsafe {
            obby_client_handle_bytes(
                client,
                welcome.to_bytes().as_ptr(),
                welcome.to_bytes().len(),
            );
        }

        let event = unsafe { obby_client_poll_event(client) };
        assert!(!event.is_null());
        assert_eq!(
            unsafe { obby_event_get_kind(event) },
            ObbyEventKind::Registered
        );
        let nick_field = unsafe { obby_event_text(event, ObbyEventField::Nick) };
        assert_eq!(unsafe { CStr::from_ptr(nick_field) }, c"typed");
        assert!(unsafe { obby_event_text(event, ObbyEventField::Account) }.is_null());
        assert!(!unsafe { obby_event_json(event) }.is_null());
        unsafe { obby_event_free(event) };

        assert!(unsafe { obby_client_join(client, c"#obby".as_ptr(), ptr::null()) });
        assert!(unsafe { obby_client_send_message(client, c"#obby".as_ptr(), c"hello".as_ptr()) });
        assert!(unsafe { obby_client_quit(client, c"bye".as_ptr()) });
        let sent = drain(client);
        assert!(sent.contains("JOIN #obby"));
        assert!(sent.contains("PRIVMSG #obby hello"));
        assert!(sent.contains("QUIT bye"));

        unsafe { obby_client_free(client) };
    }

    #[test]
    fn a_command_with_a_missing_string_is_refused_rather_than_sent() {
        let nick = c"typed";
        let settings = config(nick);
        let client = unsafe { obby_client_new(&raw const settings) };
        assert!(!unsafe { obby_client_join(client, ptr::null(), ptr::null()) });
        assert!(!unsafe { obby_client_send_message(client, c"#obby".as_ptr(), ptr::null()) });
        assert!(!unsafe { obby_client_join(ptr::null_mut(), c"#obby".as_ptr(), ptr::null()) });
        unsafe { obby_client_free(client) };
    }

    #[test]
    fn an_event_read_after_the_kind_it_does_not_have_answers_absent() {
        assert_eq!(
            unsafe { obby_event_get_kind(ptr::null()) },
            ObbyEventKind::Unknown
        );
        assert!(unsafe { obby_event_text(ptr::null(), ObbyEventField::Nick) }.is_null());
        let mut out = 0;
        assert!(!unsafe { obby_event_number(ptr::null(), ObbyEventField::AfterMs, &raw mut out) });
        unsafe { obby_event_free(ptr::null_mut()) };
    }

    #[test]
    fn a_null_config_pointer_fails_to_construct_a_client() {
        assert!(unsafe { obby_client_new(ptr::null()) }.is_null());
        assert!(unsafe { obby_client_new_from_json(ptr::null()) }.is_null());
    }

    #[test]
    fn invalid_utf8_never_reaches_the_json_parser() {
        let bad = CString::new(vec![0xFF, 0xFE, b'{']).unwrap();
        assert!(unsafe { obby_client_new_from_json(bad.as_ptr()) }.is_null());

        let client = unsafe { obby_client_new_from_json(config_json("badutf8").as_ptr()) };
        assert!(!unsafe { obby_client_command_from_json(client, bad.as_ptr()) });
        unsafe { obby_client_free(client) };
    }

    #[test]
    fn malformed_json_is_rejected_not_panicked_on() {
        let not_json = CString::new("not json at all").unwrap();
        assert!(unsafe { obby_client_new_from_json(not_json.as_ptr()) }.is_null());

        let wrong_shape = CString::new(r#"{"nick":123}"#).unwrap();
        assert!(unsafe { obby_client_new_from_json(wrong_shape.as_ptr()) }.is_null());

        let client = unsafe { obby_client_new_from_json(config_json("badjson").as_ptr()) };
        assert!(!unsafe { obby_client_command_from_json(client, not_json.as_ptr()) });

        let unknown_command = CString::new(r#"{"type":"not-a-real-command"}"#).unwrap();
        assert!(!unsafe { obby_client_command_from_json(client, unknown_command.as_ptr()) });

        unsafe { obby_client_free(client) };
    }

    #[test]
    fn freeing_the_empty_buffer_and_a_null_string_is_a_no_op() {
        unsafe {
            obby_client_free_bytes(ObbyBytes::empty());
            obby_client_free_string(ptr::null_mut());
        }
    }
}
