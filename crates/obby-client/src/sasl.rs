//! SASL authentication.
//!
//! The exchange runs inside capability negotiation: `CAP END` must not be sent while it is in
//! flight, because a server that sees it aborts with 906 and registers the connection
//! unauthenticated.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

/// The most base64 one `AUTHENTICATE` line may carry.
const CHUNK: usize = 400;

/// What to authenticate with.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum Credentials {
    /// A username and password in the clear, so only over TLS.
    Plain {
        /// The account to log in as.
        username: String,
        /// Its password.
        password: String,
    },
    /// Authenticate with the TLS client certificate the host already presented.
    External,
    /// Prove we know the password without sending it, per RFC 7677.
    ///
    /// Preferred over [`Credentials::Plain`] wherever the server offers it: the password never
    /// crosses the wire, a recording of the exchange cannot be replayed, and the server has to
    /// prove it knows the password too.
    Scram {
        /// The account to log in as.
        username: String,
        /// Its password.
        password: String,
        /// Unpredictable bytes, never reused. The core has no entropy source, so the host supplies
        /// this, and reusing one destroys the replay protection the mechanism exists for.
        nonce: String,
    },
}

impl core::fmt::Debug for Credentials {
    /// Deliberately hand-written: a derived one would print the password, and this type is reachable
    /// from `Config`, which a host is likely to log while working out why a connection failed.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Plain { username, .. } => f
                .debug_struct("Plain")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
            Self::External => f.write_str("External"),
            Self::Scram { username, .. } => f
                .debug_struct("Scram")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish_non_exhaustive(),
        }
    }
}

impl Credentials {
    /// The mechanism name to send in the opening `AUTHENTICATE`.
    pub fn mechanism(&self) -> &'static str {
        match self {
            Self::Plain { .. } => "PLAIN",
            Self::External => "EXTERNAL",
            Self::Scram { .. } => "SCRAM-SHA-256",
        }
    }

    /// The response payload, before base64 and chunking.
    ///
    /// Both mechanisms leave the authorisation identity empty, which asks the server to authorise as
    /// whoever we authenticated as.
    pub fn response(&self) -> Vec<u8> {
        match self {
            Self::Plain { username, password } => {
                let mut out = Vec::new();
                out.push(0);
                out.extend_from_slice(username.as_bytes());
                out.push(0);
                out.extend_from_slice(password.as_bytes());
                out
            }
            Self::External | Self::Scram { .. } => Vec::new(),
        }
    }

    /// True when this mechanism needs more than one round trip.
    pub fn is_challenge_response(&self) -> bool {
        matches!(self, Self::Scram { .. })
    }
}

/// Split a response into the payloads of consecutive `AUTHENTICATE` lines.
///
/// An empty response is a single `+`. A response whose base64 is an exact multiple of the chunk size
/// is followed by a `+`, because otherwise the server cannot tell the last full chunk from a
/// continuation and waits forever.
pub(crate) fn encode_response(payload: &[u8]) -> Vec<String> {
    if payload.is_empty() {
        return alloc::vec!["+".to_string()];
    }
    let encoded = BASE64.encode(payload);
    let mut lines: Vec<String> = encoded
        .as_bytes()
        .chunks(CHUNK)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect();
    if encoded.len().is_multiple_of(CHUNK) {
        lines.push("+".to_string());
    }
    lines
}

/// Decode a server challenge. `+` means an empty challenge.
pub(crate) fn decode_challenge(payload: &str) -> Option<Vec<u8>> {
    if payload == "+" {
        return Some(Vec::new());
    }
    BASE64.decode(payload).ok()
}

/// True when the server's advertised mechanism list contains this one.
///
/// An absent or empty list means the server did not say, in which case we try and let it answer.
pub(crate) fn offers(advertised: Option<&str>, mechanism: &str) -> bool {
    match advertised {
        None | Some("") => true,
        Some(list) => list.split(',').any(|m| m.eq_ignore_ascii_case(mechanism)),
    }
}

/// Why authentication ended without succeeding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum SaslFailure {
    /// 904: the credentials were rejected.
    Rejected,
    /// 905: the response was longer than the server accepts.
    TooLong,
    /// 906: the exchange was aborted before it finished.
    Aborted,
    /// 907: this connection already authenticated.
    AlreadyAuthenticated,
    /// The server offers no mechanism we can speak.
    NoSharedMechanism,
    /// The server could not prove it knows the password, so it is not the server it claims.
    ServerNotVerified,
}

/// How far authentication has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SaslState {
    /// Not started, either because there are no credentials or the server offers no `sasl`.
    #[default]
    Idle,
    /// `AUTHENTICATE <mech>` is sent, waiting for the server to invite the response.
    Offered,
    /// The response is sent, waiting for a verdict.
    Responded,
    /// SCRAM: our first message is sent, waiting for the salt and the server's nonce.
    ScramChallenged,
    /// SCRAM: our proof is sent, waiting for the server to prove itself in return.
    ScramProved,
    /// Finished, one way or the other. Negotiation may now end.
    Settled,
}

impl SaslState {
    /// True while `CAP END` must be held back.
    pub(crate) fn in_flight(self) -> bool {
        matches!(
            self,
            Self::Offered | Self::Responded | Self::ScramChallenged | Self::ScramProved
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_frames_the_response_with_null_separators() {
        let creds = Credentials::Plain {
            username: "alice".to_string(),
            password: "hunter2".to_string(),
        };
        assert_eq!(creds.mechanism(), "PLAIN");
        assert_eq!(creds.response(), b"\0alice\0hunter2");
    }

    #[test]
    fn external_sends_an_empty_response() {
        assert_eq!(Credentials::External.mechanism(), "EXTERNAL");
        assert_eq!(encode_response(&Credentials::External.response()), ["+"]);
    }

    #[test]
    fn a_short_response_is_one_line() {
        let lines = encode_response(b"\0alice\0hunter2");
        assert_eq!(lines.len(), 1);
        assert_eq!(
            decode_challenge(&lines[0]).as_deref(),
            Some(&b"\0alice\0hunter2"[..])
        );
    }

    #[test]
    fn a_long_response_splits_at_four_hundred() {
        // 300 bytes encode to exactly 400 base64 characters, so one more byte spills to a second line
        let lines = encode_response(&[b'x'; 301]);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), CHUNK);
        assert!(lines[1].len() < CHUNK);
    }

    #[test]
    fn an_exact_multiple_gets_a_trailing_plus() {
        // without this the server cannot tell a final full chunk from a continuation
        let lines = encode_response(&[b'x'; 300]);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), CHUNK);
        assert_eq!(lines[1], "+");
    }

    #[test]
    fn the_chunks_rejoin_into_the_original() {
        let payload: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let joined: String = encode_response(&payload)
            .into_iter()
            .filter(|line| line != "+")
            .collect();
        assert_eq!(decode_challenge(&joined).as_deref(), Some(&payload[..]));
    }

    #[test]
    fn an_empty_challenge_is_a_plus() {
        assert_eq!(decode_challenge("+"), Some(Vec::new()));
        assert_eq!(decode_challenge("not base64!"), None);
    }

    #[test]
    fn an_unadvertised_mechanism_list_is_not_a_refusal() {
        assert!(
            offers(None, "PLAIN"),
            "no list means the server did not say"
        );
        assert!(offers(Some(""), "PLAIN"));
        assert!(offers(Some("PLAIN,EXTERNAL"), "EXTERNAL"));
        assert!(
            offers(Some("plain"), "PLAIN"),
            "mechanism names are case-insensitive"
        );
        assert!(!offers(Some("SCRAM-SHA-256"), "PLAIN"));
    }

    #[test]
    fn cap_end_waits_only_while_the_exchange_runs() {
        assert!(!SaslState::Idle.in_flight());
        assert!(SaslState::Offered.in_flight());
        assert!(SaslState::Responded.in_flight());
        assert!(
            SaslState::ScramChallenged.in_flight() && SaslState::ScramProved.in_flight(),
            "a challenge-response mechanism takes several rounds, and CAP END must wait for all of them"
        );
        assert!(!SaslState::Settled.in_flight());
    }

    #[test]
    fn a_password_never_appears_in_a_debug_rendering() {
        let plain = Credentials::Plain {
            username: "alice".to_string(),
            password: "hunter2".to_string(),
        };
        let rendered = alloc::format!("{plain:?}");
        assert!(!rendered.contains("hunter2"), "got: {rendered}");
        assert!(rendered.contains("alice"), "the username is not the secret");

        let scram = Credentials::Scram {
            username: "alice".to_string(),
            password: "hunter2".to_string(),
            nonce: "abc".to_string(),
        };
        assert!(!alloc::format!("{scram:?}").contains("hunter2"));
    }

    #[test]
    fn scram_is_a_challenge_response_mechanism_and_the_others_are_not() {
        let scram = Credentials::Scram {
            username: "alice".to_string(),
            password: "hunter2".to_string(),
            nonce: "abc".to_string(),
        };
        assert_eq!(scram.mechanism(), "SCRAM-SHA-256");
        assert!(scram.is_challenge_response());
        assert!(!Credentials::External.is_challenge_response());
    }
}
