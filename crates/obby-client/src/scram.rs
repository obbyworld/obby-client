//! SCRAM-SHA-256, from RFC 5802 and RFC 7677.
//!
//! The password never crosses the wire. Each side proves it knows a key derived from the password
//! and a salt the server chose, over a nonce both sides contributed to, so a recording of the
//! exchange cannot be replayed and a server compromise does not hand over the password itself.
//!
//! Only the `SCRAM-SHA-256` mechanism without channel binding is implemented, which is the one IRC
//! servers offer. The `n,,` prefix below is the GS2 header saying exactly that.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// How long the nonce we contribute is, in characters.
const NONCE_LEN: usize = 24;

/// The most iterations we will run.
///
/// The count comes from the server, so a hostile one could otherwise ask for billions and hang the
/// client in a loop it cannot interrupt.
const MAX_ITERATIONS: u32 = 1_000_000;

/// Why an exchange could not continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum ScramError {
    /// The server's message was not the shape the specification defines.
    Malformed,
    /// The server's nonce does not start with the one we sent, so it is not answering us.
    NonceMismatch,
    /// The server asked for more iterations than we will run.
    TooManyIterations,
    /// The salt was not valid base64.
    BadSalt,
    /// The server could not prove it knows the password, so it is not the server it claims.
    ServerProofInvalid,
}

/// An exchange in progress.
///
/// Holds what the first message committed to, so the second can be checked against it.
pub struct Scram {
    /// What we sent, minus the GS2 header, which the signature is computed over.
    client_first_bare: String,
    /// Our half of the nonce, which the server must echo back.
    client_nonce: String,
    password: String,
    /// Set once we have computed it, so the server's own proof can be checked.
    server_key: Option<Vec<u8>>,
    auth_message: Option<String>,
}

impl core::fmt::Debug for Scram {
    /// Deliberately hand-written: a derived one would print the password into whatever log the
    /// caller happens to write, and this type is reachable from `Client`, which is `Debug`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Scram")
            .field("client_nonce", &self.client_nonce)
            .field("password", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl Scram {
    /// Start an exchange.
    ///
    /// `nonce` must be unpredictable and never reused: it is what stops a recorded exchange being
    /// replayed. The core has no entropy source, so the host supplies it.
    pub fn new(username: &str, password: &str, nonce: &str) -> Self {
        let client_nonce = sanitise_nonce(nonce);
        let client_first_bare = alloc::format!("n={},r={client_nonce}", saslprep(username));
        Self {
            client_first_bare,
            client_nonce,
            password: password.to_string(),
            server_key: None,
            auth_message: None,
        }
    }

    /// The first message to send, including the GS2 header saying we do not use channel binding.
    pub fn client_first(&self) -> Vec<u8> {
        alloc::format!("n,,{}", self.client_first_bare).into_bytes()
    }

    /// Answer the server's first message with our proof.
    pub fn client_final(&mut self, server_first: &[u8]) -> Result<Vec<u8>, ScramError> {
        let server_first = core::str::from_utf8(server_first).map_err(|_| ScramError::Malformed)?;
        let nonce = field(server_first, 'r').ok_or(ScramError::Malformed)?;
        let salt = field(server_first, 's').ok_or(ScramError::Malformed)?;
        let iterations: u32 = field(server_first, 'i')
            .ok_or(ScramError::Malformed)?
            .parse()
            .map_err(|_| ScramError::Malformed)?;

        // the server extends our nonce rather than replacing it, and a server that does not is
        // either confused or replaying somebody else's exchange at us
        if !nonce.starts_with(&self.client_nonce) || nonce == self.client_nonce {
            return Err(ScramError::NonceMismatch);
        }
        if iterations == 0 || iterations > MAX_ITERATIONS {
            return Err(ScramError::TooManyIterations);
        }
        let salt = base64_decode(salt).ok_or(ScramError::BadSalt)?;

        let salted =
            hi(self.password.as_bytes(), &salt, iterations).ok_or(ScramError::Malformed)?;
        let client_key = hmac(&salted, b"Client Key").ok_or(ScramError::Malformed)?;
        let stored_key = Sha256::digest(&client_key);
        self.server_key = Some(hmac(&salted, b"Server Key").ok_or(ScramError::Malformed)?);

        // `c=biws` is base64 of the `n,,` header we opened with, echoed so the server can check
        // that nobody rewrote it in flight
        let client_final_bare = alloc::format!("c=biws,r={nonce}");
        let auth_message = alloc::format!(
            "{},{server_first},{client_final_bare}",
            self.client_first_bare
        );

        let client_signature =
            hmac(&stored_key, auth_message.as_bytes()).ok_or(ScramError::Malformed)?;
        let proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature.iter())
            .map(|(key, signature)| key ^ signature)
            .collect();

        self.auth_message = Some(auth_message);
        Ok(alloc::format!("{client_final_bare},p={}", base64_encode(&proof)).into_bytes())
    }

    /// Check the server's closing message.
    ///
    /// This is the half most implementations skip, and skipping it means a server that never knew
    /// the password can still convince the client it did.
    pub fn verify(&self, server_final: &[u8]) -> Result<(), ScramError> {
        let server_final = core::str::from_utf8(server_final).map_err(|_| ScramError::Malformed)?;
        let signature = field(server_final, 'v').ok_or(ScramError::Malformed)?;
        let (Some(server_key), Some(auth_message)) = (&self.server_key, &self.auth_message) else {
            return Err(ScramError::Malformed);
        };
        let expected = hmac(server_key, auth_message.as_bytes()).ok_or(ScramError::Malformed)?;
        let given = base64_decode(signature).ok_or(ScramError::Malformed)?;
        if constant_time_eq(&expected, &given) {
            Ok(())
        } else {
            Err(ScramError::ServerProofInvalid)
        }
    }
}

/// HMAC-SHA-256.
///
/// `None` only if the key length were rejected, which HMAC never does, so callers propagate it
/// rather than assert it away.
fn hmac(key: &[u8], message: &[u8]) -> Option<Vec<u8>> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).ok()?;
    mac.update(message);
    Some(mac.finalize().into_bytes().to_vec())
}

/// `Hi` from RFC 5802: the password stretched over `iterations` rounds of HMAC.
fn hi(password: &[u8], salt: &[u8], iterations: u32) -> Option<Vec<u8>> {
    let mut previous = hmac(password, &[salt, &[0, 0, 0, 1]].concat())?;
    let mut result = previous.clone();
    for _ in 1..iterations {
        previous = hmac(password, &previous)?;
        for (accumulated, round) in result.iter_mut().zip(previous.iter()) {
            *accumulated ^= round;
        }
    }
    Some(result)
}

/// Read one `k=value` field out of a comma-separated SCRAM message.
fn field(message: &str, key: char) -> Option<&str> {
    message
        .split(',')
        .find_map(|part| part.strip_prefix(key)?.strip_prefix('='))
}

/// Strip what a nonce may not contain.
///
/// A comma would end the field early and let a caller's nonce forge the rest of the message.
fn sanitise_nonce(nonce: &str) -> String {
    let cleaned: String = nonce
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(NONCE_LEN)
        .collect();
    if cleaned.is_empty() {
        "0".to_string()
    } else {
        cleaned
    }
}

/// Escape the two characters a username may not carry literally.
///
/// A `,` or `=` would otherwise end the field or start a new one, letting a username rewrite the
/// message around it.
fn saslprep(username: &str) -> String {
    username.replace('=', "=3D").replace(',', "=2C")
}

/// Compare without letting the time taken reveal where two values first differ.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |difference, (x, y)| difference | (x ^ y))
        == 0
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worked example from RFC 7677 section 3.
    const USER: &str = "user";
    const PASSWORD: &str = "pencil";
    const CLIENT_NONCE: &str = "rOprNGfwEbeRWgbNEkqO";
    const SERVER_FIRST: &str =
        "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";

    #[test]
    fn the_first_message_carries_the_user_and_our_nonce() {
        let scram = Scram::new(USER, PASSWORD, CLIENT_NONCE);
        assert_eq!(
            scram.client_first(),
            b"n,,n=user,r=rOprNGfwEbeRWgbNEkqO".to_vec()
        );
    }

    #[test]
    fn the_proof_matches_the_published_example() {
        let mut scram = Scram::new(USER, PASSWORD, CLIENT_NONCE);
        let final_message = scram
            .client_final(SERVER_FIRST.as_bytes())
            .expect("the example server message is well formed");
        assert_eq!(
            core::str::from_utf8(&final_message).expect("utf8"),
            "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=",
            "RFC 7677 publishes this exact proof, so a mismatch means the derivation is wrong"
        );
    }

    #[test]
    fn the_servers_own_proof_is_checked() {
        let mut scram = Scram::new(USER, PASSWORD, CLIENT_NONCE);
        scram
            .client_final(SERVER_FIRST.as_bytes())
            .expect("client final");
        assert_eq!(
            scram.verify(b"v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4="),
            Ok(()),
            "the published server signature must be accepted"
        );
    }

    #[test]
    fn a_server_that_never_knew_the_password_is_rejected() {
        let mut scram = Scram::new(USER, PASSWORD, CLIENT_NONCE);
        scram
            .client_final(SERVER_FIRST.as_bytes())
            .expect("client final");
        assert_eq!(
            scram.verify(b"v=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),
            Err(ScramError::ServerProofInvalid),
            "skipping this check is how a fake server convinces a client it is real"
        );
    }

    #[test]
    fn a_server_that_does_not_extend_our_nonce_is_not_answering_us() {
        let mut scram = Scram::new(USER, PASSWORD, CLIENT_NONCE);
        assert_eq!(
            scram.client_final(b"r=somebodyelsesnonce,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096"),
            Err(ScramError::NonceMismatch)
        );
        assert_eq!(
            scram.client_final(
                alloc::format!("r={CLIENT_NONCE},s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096").as_bytes()
            ),
            Err(ScramError::NonceMismatch),
            "the server has to contribute its own half, not merely echo ours"
        );
    }

    #[test]
    fn an_absurd_iteration_count_is_refused_rather_than_run() {
        let mut scram = Scram::new(USER, PASSWORD, CLIENT_NONCE);
        assert_eq!(
            scram.client_final(
                alloc::format!("r={CLIENT_NONCE}x,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4000000000")
                    .as_bytes()
            ),
            Err(ScramError::TooManyIterations),
            "the count comes from the server, so it must be bounded on our side"
        );
        assert_eq!(
            scram.client_final(
                alloc::format!("r={CLIENT_NONCE}x,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=0").as_bytes()
            ),
            Err(ScramError::TooManyIterations)
        );
    }

    #[test]
    fn a_malformed_server_message_is_refused() {
        let mut scram = Scram::new(USER, PASSWORD, CLIENT_NONCE);
        assert_eq!(scram.client_final(b""), Err(ScramError::Malformed));
        assert_eq!(scram.client_final(b"nonsense"), Err(ScramError::Malformed));
        assert_eq!(
            scram.client_final(b"r=abc,s=notbase64!,i=4096"),
            Err(ScramError::NonceMismatch),
            "the nonce is checked before the salt is even decoded"
        );
    }

    #[test]
    fn verifying_before_the_exchange_has_run_fails_rather_than_passing() {
        let scram = Scram::new(USER, PASSWORD, CLIENT_NONCE);
        assert_eq!(scram.verify(b"v=anything"), Err(ScramError::Malformed));
    }

    #[test]
    fn a_username_cannot_rewrite_the_message_around_it() {
        let scram = Scram::new("ev,il=user", PASSWORD, CLIENT_NONCE);
        let first = String::from_utf8(scram.client_first()).expect("utf8");
        assert!(first.contains("n=ev=2Cil=3Duser"));
        assert_eq!(
            first.matches(',').count(),
            3,
            "the header's two commas plus the one before r=, and none smuggled in by the name"
        );
    }

    #[test]
    fn a_nonce_cannot_smuggle_a_field_separator() {
        let scram = Scram::new(USER, PASSWORD, "abc,r=evil");
        assert_eq!(scram.client_nonce, "abcrevil");
    }

    #[test]
    fn an_empty_nonce_still_produces_a_usable_one() {
        let scram = Scram::new(USER, PASSWORD, ",,,");
        assert!(!scram.client_nonce.is_empty());
    }

    #[test]
    fn comparison_does_not_leak_where_two_values_differ() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
