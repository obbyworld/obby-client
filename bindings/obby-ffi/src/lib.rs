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
//! [`Command`] is small and rarely constructed, compared to how often [`obby_client::Event`] grows,
//! so the two directions cross the boundary differently:
//! [`obby_client_command`] takes one JSON object per call, while [`obby_client_poll_events`] drains
//! every pending event as one JSON array per call, a single crossing rather than one per event.
//! Bytes never enter that JSON channel: [`obby_client_handle_bytes`] and
//! [`obby_client_poll_transmit`] carry them as a pointer and a length, exactly as `handle_bytes` and
//! `poll_transmit` are already separate from `poll_event` in the Rust API.
//!
//! This crate is the one place `unsafe` lives in the workspace. Every function that takes a raw
//! pointer documents exactly what the caller must guarantee, and none of them panics: a null
//! pointer, a bad length, invalid UTF-8 or malformed JSON all return an error code or a null result
//! instead.

use std::ffi::{CStr, CString, c_char};
use std::ptr;
use std::sync::OnceLock;

use obby_client::{Client, Command, Config, Now};

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
/// `client` must be null or a pointer returned by [`obby_client_new`] that has not since been
/// passed to [`obby_client_free`].
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

/// Create a client from a JSON-encoded [`Config`].
///
/// Returns null when `config_json` is null, is not valid UTF-8, or does not parse as a `Config`.
///
/// # Safety
/// `config_json` must be null or point to a NUL-terminated, valid UTF-8 C string, valid for reads
/// for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_new(config_json: *const c_char) -> *mut ObbyClient {
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

/// Tell the engine the transport is up. See [`obby_client::Client::connected`].
///
/// # Safety
/// `client` must be null or a valid, non-freed pointer from [`obby_client_new`].///
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_connected(client: *mut ObbyClient) {
    if let Some(client) = unsafe { as_client(client) } {
        client.connected();
    }
}

/// Tell the engine its transport died. See [`obby_client::Client::disconnected`].
///
/// # Safety
/// `client` must be null or a valid, non-freed pointer from [`obby_client_new`].///
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_disconnected(client: *mut ObbyClient) {
    if let Some(client) = unsafe { as_client(client) } {
        client.disconnected();
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
/// `client` must be null or a valid, non-freed pointer from [`obby_client_new`].///
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
/// `client` must be null or a valid, non-freed pointer from [`obby_client_new`].///
///
/// The handle must not be in use on another thread while this call runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn obby_client_poll_events(client: *mut ObbyClient) -> *mut c_char {
    let Some(client) = (unsafe { as_client(client) }) else {
        return ptr::null_mut();
    };
    let mut events = Vec::new();
    while let Some(event) = client.poll_event() {
        events.push(event);
    }
    serde_json::to_string(&events).map_or_else(|_| ptr::null_mut(), string_out)
}

/// Free a string returned by [`obby_client_poll_events`] or [`obby_client_model_json`].
///
/// The only legal way to release one. Never pass the pointer returned by [`obby_client_version`]
/// here: that one is static and owned by the library, not by the caller.
///
/// # Safety
/// `s` must be null, or a pointer returned by [`obby_client_poll_events`] or
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
/// `client` must be null or a valid, non-freed pointer from [`obby_client_new`].///
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

/// Submit a command from a JSON-encoded [`Command`].
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
pub unsafe extern "C" fn obby_client_command(
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
/// `client` must be null or a valid, non-freed pointer from [`obby_client_new`].///
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
        let raw = unsafe { obby_client_poll_events(client) };
        assert!(!raw.is_null(), "a live client always produces JSON");
        let text = unsafe { CStr::from_ptr(raw) }.to_str().unwrap().to_string();
        unsafe { obby_client_free_string(raw) };
        serde_json::from_str(&text).expect("poll_events always emits a JSON array")
    }

    #[test]
    fn drives_the_whole_surface_through_the_c_api() {
        let config = config_json("tester");
        let client = unsafe { obby_client_new(config.as_ptr()) };
        assert!(!client.is_null());

        unsafe { obby_client_connected(client) };
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

        let join = CString::new(r##"{"command":"join","channel":"#test","key":null}"##).unwrap();
        assert!(unsafe { obby_client_command(client, join.as_ptr()) });
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

        unsafe { obby_client_disconnected(client) };
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
            obby_client_connected(ptr::null_mut());
            obby_client_disconnected(ptr::null_mut());
            obby_client_handle_bytes(ptr::null_mut(), ptr::null(), 0);
            obby_client_handle_bytes(ptr::null_mut(), b"x".as_ptr(), 1);
            obby_client_tick(ptr::null_mut(), 0, 0);

            let bytes = obby_client_poll_transmit(ptr::null_mut());
            assert!(bytes.ptr.is_null());
            assert_eq!(bytes.len, 0);

            assert!(obby_client_poll_events(ptr::null_mut()).is_null());

            let mut out_ms = 42u64;
            assert!(!obby_client_poll_timeout(ptr::null_mut(), &raw mut out_ms));
            assert_eq!(
                out_ms, 0,
                "false is paired with a defined value, never garbage"
            );

            let ok = CString::new(r#"{"command":"quit","reason":null}"#).unwrap();
            assert!(!obby_client_command(ptr::null_mut(), ok.as_ptr()));

            assert!(obby_client_model_json(ptr::null_mut()).is_null());
        }
    }

    #[test]
    fn a_null_data_pointer_with_a_nonzero_length_is_ignored_not_dereferenced() {
        let config = config_json("nully");
        let client = unsafe { obby_client_new(config.as_ptr()) };
        unsafe {
            obby_client_handle_bytes(client, ptr::null(), 10);
            obby_client_handle_bytes(client, ptr::null(), 0);
            obby_client_free(client);
        }
    }

    #[test]
    fn a_null_out_ms_is_never_written_through() {
        let client = unsafe { obby_client_new(config_json("nully").as_ptr()) };
        unsafe {
            assert!(!obby_client_poll_timeout(client, ptr::null_mut()));
            obby_client_free(client);
        }
    }

    #[test]
    fn a_null_config_pointer_fails_to_construct_a_client() {
        assert!(unsafe { obby_client_new(ptr::null()) }.is_null());
    }

    #[test]
    fn invalid_utf8_never_reaches_the_json_parser() {
        let bad = CString::new(vec![0xFF, 0xFE, b'{']).unwrap();
        assert!(unsafe { obby_client_new(bad.as_ptr()) }.is_null());

        let client = unsafe { obby_client_new(config_json("badutf8").as_ptr()) };
        assert!(!unsafe { obby_client_command(client, bad.as_ptr()) });
        unsafe { obby_client_free(client) };
    }

    #[test]
    fn malformed_json_is_rejected_not_panicked_on() {
        let not_json = CString::new("not json at all").unwrap();
        assert!(unsafe { obby_client_new(not_json.as_ptr()) }.is_null());

        let wrong_shape = CString::new(r#"{"nick":123}"#).unwrap();
        assert!(unsafe { obby_client_new(wrong_shape.as_ptr()) }.is_null());

        let client = unsafe { obby_client_new(config_json("badjson").as_ptr()) };
        assert!(!unsafe { obby_client_command(client, not_json.as_ptr()) });

        let unknown_command = CString::new(r#"{"command":"not-a-real-command"}"#).unwrap();
        assert!(!unsafe { obby_client_command(client, unknown_command.as_ptr()) });

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
