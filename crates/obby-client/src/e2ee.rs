//! X3DH key agreement and the Double Ratchet, implemented against the wire the running client and
//! server actually speak rather than against the published
//! <https://github.com/obbyworld/extensions/blob/main/e2ee.md>. The two disagree,
//! and the clearest case is the spec's own example,
//! which inlines a top-level `ik` field on the `init` frame that the real wire never sends, only
//! the nested [`PreKeyBundle`] does.
//!
//! This module is `no_std` plus `alloc`, has no entropy source of its own, and never touches
//! `std::`, `getrandom`, or any system RNG: every function that needs randomness takes one
//! through [`RandomSource`], which the host fills from whatever CSPRNG it already holds. That
//! keeps this buildable for `wasm32-unknown-unknown`, which has no ambient entropy to reach for.
//!
//! Wire encoding (base64, JSON, the `?obe2ee:` body marker, tag fragmentation over the wire) is
//! deliberately out of scope: the types here model the frame set's fields with the research doc's
//! exact names, but turning them into bytes on a `TAGMSG`/`PRIVMSG` is the transport's job, done
//! elsewhere. For the same reason, the AEAD associated data authenticates a canonical binary
//! encoding of a message header (`dh || pn || n`) rather than literal wire JSON bytes; a future
//! wire codec must reproduce the same JSON bytes the reference client authenticates to interop
//! with it, but that does not change anything this module does with the fields once decoded.
//!
//! Not wired into `client.rs` or `session.rs` yet, so nothing outside this module's own tests
//! calls into it. The `expect` below is scoped to non-test builds specifically so that wiring
//! this in later without removing the annotation fails loudly (an unfulfilled expectation)
//! instead of silently doing nothing, which is what `#[allow(dead_code)]` would do.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::mem;

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hkdf::Hkdf;
use hkdf::hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

/// The wire's protocol version, carried as `v` on every frame. Never varies today; kept as a
/// named constant for whoever writes the wire codec rather than a magic `1` in two places.
pub const PROTOCOL_VERSION: u32 = 1;

/// One skip-ahead jump's bound: a single message whose counter implies more than this many
/// unseen messages in one chain is refused outright, rather than buffered.
pub const MAX_SKIP: u32 = 1000;

/// The total number of out-of-order message keys this ratchet will hold onto at once, across
/// every chain it has ever had. Beyond this, the oldest key is evicted to make room, so a peer
/// cannot exhaust memory by never sending the messages a lower counter promised.
pub const MAX_SKIPPED_KEYS: usize = 2000;

/// Plaintext is padded to a multiple of this many bytes before encryption, so ciphertext length
/// reveals only a size bucket rather than the exact message length.
const PAD_BLOCK: usize = 64;

const X3DH_INFO: &[u8] = b"obby.world/e2ee x3dh";
const ROOT_INFO: &[u8] = b"obby.world/e2ee root";
const MESSAGE_KEY_INFO: &[u8] = b"obby.world/e2ee message";
const NONCE_INFO: &[u8] = b"obby.world/e2ee nonce";

/// Everything that can go wrong here: a doomed handshake, a ratchet that refuses to advance, or
/// a caller asking the state machine for a transition it does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "E2eeError"))]
pub enum Error {
    /// A signature the protocol requires did not verify.
    InvalidSignature,
    /// A Diffie-Hellman output was non-contributory: a small-order or otherwise degenerate
    /// public key, which HKDF would otherwise turn into a predictable key.
    NonContributoryDh,
    /// An AEAD operation failed: on decrypt, a tampered, misrouted, or out-of-session
    /// ciphertext; on encrypt, only ever a coding error in this module.
    Aead,
    /// A decrypted plaintext's padding was not well-formed ISO/IEC 7816-4 padding.
    Padding,
    /// A message implied a skip-ahead of more than [`MAX_SKIP`] messages in one jump.
    TooManySkipped,
    /// A message counter would overflow `u32`, which cannot happen in a real conversation and
    /// therefore signals a malicious or corrupted counter.
    CounterOverflow,
    /// There is no established sending or receiving chain to use yet.
    NoChain,
    /// The peer's fingerprint changed since it was first pinned. The caller must call
    /// [`Session::confirm_fingerprint_change`] before the same operation can succeed.
    FingerprintChanged {
        /// The fingerprint pinned from an earlier conversation.
        previous: Fingerprint,
        /// The fingerprint just observed.
        current: Fingerprint,
    },
    /// The session is not in a state that allows this operation, such as decrypting content
    /// before the handshake's `ack` has been received and decrypted.
    WrongState,
    /// A `frag` set could not be reassembled: a mismatched `id`/`n`, a duplicate or
    /// out-of-range index, or a missing piece.
    Fragmentation,
    /// An underlying primitive rejected an input this module always constructs to be valid
    /// (an HMAC key length, an HKDF output length). Never expected to occur in practice.
    Internal,
}

/// A source of random bytes, supplied by the caller.
///
/// This module has no entropy source of its own: no OS RNG, no `getrandom`, nothing that would
/// need a host bridge on a target like `wasm32-unknown-unknown` where none exists. A caller
/// fills this from a CSPRNG it already holds, or from a fixed seed for reproducible tests.
pub trait RandomSource {
    /// Fill `dest` with bytes suitable for key material.
    fn fill_bytes(&mut self, dest: &mut [u8]);
}

fn random_array<const N: usize>(rng: &mut impl RandomSource) -> [u8; N] {
    let mut bytes = [0u8; N];
    rng.fill_bytes(&mut bytes);
    bytes
}

fn generate_x25519_keypair(rng: &mut impl RandomSource) -> ([u8; 32], [u8; 32]) {
    let secret = random_array::<32>(rng);
    let public = *PublicKey::from(&StaticSecret::from(secret)).as_bytes();
    (secret, public)
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) -> Result<(), Error> {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    hk.expand(info, out).map_err(|_| Error::Internal)
}

fn hmac_sha256(key: &[u8; 32], data: &[u8]) -> Result<[u8; 32], Error> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|_| Error::Internal)?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().into())
}

fn diffie_hellman_raw(secret: &[u8; 32], public: &[u8; 32]) -> Result<[u8; 32], Error> {
    let secret = StaticSecret::from(*secret);
    let public = PublicKey::from(*public);
    let shared = secret.diffie_hellman(&public);
    if !shared.was_contributory() {
        return Err(Error::NonContributoryDh);
    }
    Ok(*shared.as_bytes())
}

// ---------------------------------------------------------------------------------------------
// Identity, fingerprints and trust-on-first-use
// ---------------------------------------------------------------------------------------------

/// The public half of an [`Identity`]: an X25519 agreement key (`ik`) and an Ed25519 signing
/// key (`sik`), exactly as they travel inside [`PreKeyBundle`] and [`HandshakeResponse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct IdentityPublic {
    /// The X25519 identity agreement key, `ik`.
    pub agreement: [u8; 32],
    /// The Ed25519 signing key, `sik`. Every signature in this protocol verifies against this
    /// key, and [`Fingerprint`] is derived from it, never from `agreement`.
    pub signing: [u8; 32],
}

struct IdentitySecret {
    agreement: Zeroizing<[u8; 32]>,
    signing: Zeroizing<[u8; 32]>,
}

/// A long-term identity: an X25519 agreement key and an Ed25519 signing key, generated once and
/// kept for the lifetime of an account.
pub struct Identity {
    secret: IdentitySecret,
    public: IdentityPublic,
}

impl Identity {
    /// Generate a fresh identity from caller-supplied randomness.
    pub fn generate(rng: &mut impl RandomSource) -> Self {
        let agreement_secret = random_array::<32>(rng);
        let signing_secret = random_array::<32>(rng);
        let agreement_public = *PublicKey::from(&StaticSecret::from(agreement_secret)).as_bytes();
        let signing_public = *SigningKey::from_bytes(&signing_secret)
            .verifying_key()
            .as_bytes();
        Self {
            secret: IdentitySecret {
                agreement: Zeroizing::new(agreement_secret),
                signing: Zeroizing::new(signing_secret),
            },
            public: IdentityPublic {
                agreement: agreement_public,
                signing: signing_public,
            },
        }
    }

    /// The public half of this identity, safe to publish.
    pub const fn public(&self) -> IdentityPublic {
        self.public
    }

    /// This identity's own fingerprint, derived from its signing key.
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of_signing_key(&self.public.signing)
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, Error> {
        let signing_key = SigningKey::from_bytes(&self.secret.signing);
        let signature: Signature = signing_key
            .try_sign(message)
            .map_err(|_| Error::InvalidSignature)?;
        Ok(signature.to_bytes().to_vec())
    }
}

/// A peer's identity fingerprint: the first 16 bytes of `SHA-256(signing_public_key)`.
///
/// Derived from the signing key, never the agreement key, so the key whose signature is
/// verified on every handshake is provably the same key a safety number displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct Fingerprint([u8; 16]);

impl Fingerprint {
    /// Derive a fingerprint from a raw Ed25519 signing public key.
    pub fn of_signing_key(signing_public: &[u8; 32]) -> Self {
        let digest = Sha256::digest(signing_public);
        let (head, _tail) = digest.split_at(16);
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(head);
        Self(bytes)
    }

    /// Render as 8 groups of 4 uppercase hex characters separated by spaces: the safety number
    /// two people compare out of band to confirm they share the same peer.
    pub fn safety_number(&self) -> String {
        let mut out = String::with_capacity(39);
        for (index, pair) in self.0.chunks(2).enumerate() {
            if index > 0 {
                out.push(' ');
            }
            for byte in pair {
                let _ = write!(out, "{byte:02X}");
            }
        }
        out
    }
}

/// The result of observing a peer's fingerprint against what was previously pinned for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOutcome {
    /// No fingerprint was pinned yet; this one now is.
    New,
    /// The observed fingerprint matches what was already pinned.
    Same,
    /// The observed fingerprint does not match the pinned one. The pin is left unchanged;
    /// only [`PeerTrust::repin`] moves it.
    Changed {
        /// The fingerprint that stays pinned until the caller explicitly repins.
        previous: Fingerprint,
    },
}

/// Trust-on-first-use bookkeeping for one peer.
///
/// Pins a fingerprint the first time a peer is seen and never moves the pin silently: a later
/// mismatch is reported, not applied, and only [`PeerTrust::repin`] accepts a new key.
#[derive(Debug, Clone, Default)]
pub struct PeerTrust {
    pinned: Option<Fingerprint>,
    verified: bool,
}

impl PeerTrust {
    /// A trust record for a peer that has never been seen.
    pub fn new() -> Self {
        Self::default()
    }

    /// Compare `fingerprint` against the pin, recording it on first contact.
    pub fn observe(&mut self, fingerprint: Fingerprint) -> PinOutcome {
        match self.pinned {
            None => {
                self.pinned = Some(fingerprint);
                PinOutcome::New
            }
            Some(pinned) if pinned == fingerprint => PinOutcome::Same,
            Some(previous) => PinOutcome::Changed { previous },
        }
    }

    /// Explicitly accept `fingerprint` as the pin, after the caller has decided a key change is
    /// legitimate. Clears verification, since that was asserted for the old key.
    pub fn repin(&mut self, fingerprint: Fingerprint) {
        self.pinned = Some(fingerprint);
        self.verified = false;
    }

    /// The currently pinned fingerprint, if any.
    pub const fn pinned(&self) -> Option<Fingerprint> {
        self.pinned
    }

    /// Whether the pinned fingerprint has been confirmed out of band.
    pub const fn is_verified(&self) -> bool {
        self.verified
    }

    /// Record that the pinned fingerprint has (or has not) been confirmed out of band.
    pub fn set_verified(&mut self, verified: bool) {
        self.verified = verified;
    }
}

/// Decide who keeps their offer when both sides send `init` at the same moment.
///
/// The side with the lower fingerprint keeps its offer and stays the initiator; the other side
/// answers. A side that cannot read the peer's fingerprint (`peer` is `None`, an offer that
/// failed to parse) always answers, since holding in that case could deadlock both sides
/// forever.
pub fn keeps_own_offer(own: Fingerprint, peer: Option<Fingerprint>) -> bool {
    match peer {
        Some(peer) => own < peer,
        None => false,
    }
}

// ---------------------------------------------------------------------------------------------
// The wire's frame set
// ---------------------------------------------------------------------------------------------

/// A responder-published prekey bundle, decoded from the wire's base64 JSON blob that `init`
/// carries as `bundle`. Field names match the wire exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct PreKeyBundle {
    /// The sender's identity agreement key.
    pub ik: [u8; 32],
    /// The sender's identity signing key.
    pub sik: [u8; 32],
    /// A freshly generated signed-prekey.
    pub spk: [u8; 32],
    /// `Ed25519(ik ‖ spk ‖ opk)`, signed with `sik`.
    pub sig: Vec<u8>,
    /// A freshly generated one-time prekey.
    pub opk: [u8; 32],
}

/// The responder's answer, decoded from the wire's base64 JSON blob that `accept` carries as
/// `response`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct HandshakeResponse {
    /// The responder's identity agreement key.
    pub ik: [u8; 32],
    /// The responder's identity signing key.
    pub sik: [u8; 32],
    /// A freshly generated ephemeral key, doubling as the responder's first ratchet keypair.
    pub ek: [u8; 32],
    /// `Ed25519(ik ‖ ek)`, signed with `sik`.
    pub sig: Vec<u8>,
    /// The first ratchet message, carrying empty plaintext, proving the responder derived the
    /// same X3DH secret the initiator will.
    pub boot: RatchetMessage,
}

/// One Double Ratchet message: a header carried as authenticated associated data, and an AEAD
/// ciphertext.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
pub struct RatchetMessage {
    /// The sender's current ratchet public key.
    pub dh: [u8; 32],
    /// The length of the sender's previous sending chain.
    pub pn: u32,
    /// This message's counter within the sender's current chain.
    pub n: u32,
    /// The AEAD ciphertext.
    pub ct: Vec<u8>,
}

/// One `t`/`v` protocol frame, exactly as the wire's client-only tag carries it (`init`,
/// `accept`, `reject`, `ack`, `close`) or, for `msg` and `media`, the `?obe2ee:`-prefixed body.
///
/// `frag` is not a variant here: it wraps another frame's encoded bytes across several wire
/// lines and belongs to the transport that reassembles it, not to session logic. See [`Frag`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "E2eeFrame"))]
pub enum Frame {
    /// An offer to start an encrypted session.
    Init {
        /// The offering side's prekey bundle.
        bundle: PreKeyBundle,
        /// The sender's SASL account, when it has one.
        account: Option<String>,
    },
    /// An answer to an offer.
    Accept {
        /// The answering side's handshake response.
        response: HandshakeResponse,
        /// The sender's SASL account, when it has one.
        account: Option<String>,
    },
    /// A refusal of an offer.
    Reject {
        /// An optional human-readable reason.
        reason: Option<String>,
    },
    /// The initiator's first encrypted payload, proving the session works.
    Ack {
        /// The ratchet-encrypted empty payload.
        ct: RatchetMessage,
    },
    /// The end of a session.
    Close,
    /// An encrypted text message.
    Msg {
        /// The ratchet-encrypted payload.
        ct: RatchetMessage,
    },
    /// An encrypted file descriptor.
    Media {
        /// The ratchet-encrypted payload, whose plaintext is a media descriptor.
        ct: RatchetMessage,
    },
}

/// The fragmentation envelope the real client uses to split a frame too large for one wire
/// line, on either carrier. The working client sends this, and no spec text describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "obby.ts"))]
#[cfg_attr(feature = "ts", ts(rename = "E2eeFragment"))]
pub struct Frag {
    /// The id every fragment of one split frame shares.
    pub id: String,
    /// This fragment's 0-based index.
    pub i: u32,
    /// The total number of fragments in the set.
    pub n: u32,
    /// This fragment's slice of the encoded payload.
    pub ct: Vec<u8>,
}

/// Reassemble a complete set of fragments back into the payload they were split from.
///
/// Rejects a set with a mismatched `id` or `n` across fragments, a duplicate or out-of-range
/// index, or a missing piece. Buffering fragments as they trickle in, and sweeping a stream
/// that never completes, needs a clock this module is never given; that bookkeeping belongs to
/// the transport that owns `tick`, not here.
pub fn reassemble(fragments: &[Frag]) -> Result<Vec<u8>, Error> {
    let first = fragments.first().ok_or(Error::Fragmentation)?;
    let id = &first.id;
    let total = first.n;
    let expected = u32::try_from(fragments.len()).map_err(|_| Error::Fragmentation)?;
    if expected != total {
        return Err(Error::Fragmentation);
    }

    let mut slots: Vec<Option<&[u8]>> = alloc::vec![None; fragments.len()];
    for fragment in fragments {
        if &fragment.id != id || fragment.n != total {
            return Err(Error::Fragmentation);
        }
        let index = usize::try_from(fragment.i).map_err(|_| Error::Fragmentation)?;
        let slot = slots.get_mut(index).ok_or(Error::Fragmentation)?;
        if slot.is_some() {
            return Err(Error::Fragmentation);
        }
        *slot = Some(&fragment.ct);
    }

    let mut out = Vec::new();
    for slot in slots {
        out.extend_from_slice(slot.ok_or(Error::Fragmentation)?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// X3DH
// ---------------------------------------------------------------------------------------------

/// The offering side's freshly generated prekeys, retained locally until the peer's `accept`
/// arrives. Never sent as-is; [`PreKeyBundle`] is the public half that is.
pub struct PendingOffer {
    bundle: PreKeyBundle,
    spk_secret: Zeroizing<[u8; 32]>,
    opk_secret: Zeroizing<[u8; 32]>,
}

/// Build the prekey bundle an `init` frame carries: a fresh signed-prekey and one-time-prekey,
/// signed together with the identity key by the long-term signing key.
pub fn create_offer(
    identity: &Identity,
    rng: &mut impl RandomSource,
) -> Result<PendingOffer, Error> {
    let spk_secret = random_array::<32>(rng);
    let opk_secret = random_array::<32>(rng);
    let spk_public = *PublicKey::from(&StaticSecret::from(spk_secret)).as_bytes();
    let opk_public = *PublicKey::from(&StaticSecret::from(opk_secret)).as_bytes();

    let public = identity.public();
    let signed = concat(&[&public.agreement, &spk_public, &opk_public]);
    let sig = identity.sign(&signed)?;

    Ok(PendingOffer {
        bundle: PreKeyBundle {
            ik: public.agreement,
            sik: public.signing,
            spk: spk_public,
            sig,
            opk: opk_public,
        },
        spk_secret: Zeroizing::new(spk_secret),
        opk_secret: Zeroizing::new(opk_secret),
    })
}

fn verify_signature(
    signing_public: &[u8; 32],
    message: &[u8],
    signature: &[u8],
) -> Result<(), Error> {
    let verifying_key =
        VerifyingKey::from_bytes(signing_public).map_err(|_| Error::InvalidSignature)?;
    let signature = Signature::try_from(signature).map_err(|_| Error::InvalidSignature)?;
    verifying_key
        .verify_strict(message, &signature)
        .map_err(|_| Error::InvalidSignature)
}

fn verify_bundle_signature(bundle: &PreKeyBundle) -> Result<(), Error> {
    let signed = concat(&[&bundle.ik, &bundle.spk, &bundle.opk]);
    verify_signature(&bundle.sik, &signed, &bundle.sig)
}

fn x3dh_kdf(
    dh1: &[u8; 32],
    dh2: &[u8; 32],
    dh3: &[u8; 32],
    dh4: &[u8; 32],
) -> Result<Zeroizing<[u8; 32]>, Error> {
    let ikm = concat(&[dh1, dh2, dh3, dh4]);
    let mut sk = [0u8; 32];
    hkdf_sha256(&[0u8; 32], &ikm, X3DH_INFO, &mut sk)?;
    Ok(Zeroizing::new(sk))
}

/// The responder's (wire sense: the side that received `init`) X3DH shared secret.
fn x3dh_secret_responder(
    own_identity: &[u8; 32],
    own_ephemeral: &[u8; 32],
    peer_identity: &[u8; 32],
    peer_signed_prekey: &[u8; 32],
    peer_one_time_prekey: &[u8; 32],
) -> Result<Zeroizing<[u8; 32]>, Error> {
    let dh1 = diffie_hellman_raw(own_identity, peer_signed_prekey)?;
    let dh2 = diffie_hellman_raw(own_ephemeral, peer_identity)?;
    let dh3 = diffie_hellman_raw(own_ephemeral, peer_signed_prekey)?;
    let dh4 = diffie_hellman_raw(own_ephemeral, peer_one_time_prekey)?;
    x3dh_kdf(&dh1, &dh2, &dh3, &dh4)
}

/// The initiator's (wire sense: the side that sent `init`) X3DH shared secret, from the
/// opposite pairing; equal to the responder's by X25519's DH symmetry.
fn x3dh_secret_initiator(
    own_signed_prekey: &[u8; 32],
    own_identity: &[u8; 32],
    own_one_time_prekey: &[u8; 32],
    peer_identity: &[u8; 32],
    peer_ephemeral: &[u8; 32],
) -> Result<Zeroizing<[u8; 32]>, Error> {
    let dh1 = diffie_hellman_raw(own_signed_prekey, peer_identity)?;
    let dh2 = diffie_hellman_raw(own_identity, peer_ephemeral)?;
    let dh3 = diffie_hellman_raw(own_signed_prekey, peer_ephemeral)?;
    let dh4 = diffie_hellman_raw(own_one_time_prekey, peer_ephemeral)?;
    x3dh_kdf(&dh1, &dh2, &dh3, &dh4)
}

/// The responder's reaction to an inbound `init`: verify the bundle's self-signature, derive
/// the X3DH secret, and open a sending-only ratchet whose first message (`boot`) proves it
/// derived the same secret the initiator will.
///
/// The signature check happens before anything else in this function touches the bundle's keys,
/// so there is no path from an unverified bundle to a live ratchet or a pinned fingerprint.
pub fn accept_offer(
    identity: &Identity,
    bundle: &PreKeyBundle,
    rng: &mut impl RandomSource,
) -> Result<(HandshakeResponse, Ratchet), Error> {
    verify_bundle_signature(bundle)?;

    let (ek_secret, ek_public) = generate_x25519_keypair(rng);
    let sk = x3dh_secret_responder(
        &identity.secret.agreement,
        &ek_secret,
        &bundle.ik,
        &bundle.spk,
        &bundle.opk,
    )?;

    let mut ratchet = Ratchet::init_as_responder(*sk, ek_secret, ek_public, bundle.spk)?;
    let boot = ratchet.encrypt(&[])?;

    let public = identity.public();
    let signed = concat(&[&public.agreement, &ek_public]);
    let sig = identity.sign(&signed)?;

    let response = HandshakeResponse {
        ik: public.agreement,
        sik: public.signing,
        ek: ek_public,
        sig,
        boot,
    };
    Ok((response, ratchet))
}

/// The initiator's reaction to an inbound `accept`: verify the responder's signature over its
/// own ephemeral key, derive the X3DH secret, and decrypt `boot` to complete the receiving side
/// of the ratchet.
///
/// The signature check happens before this function touches `response.sik` for anything other
/// than that check, which is what makes it impossible to reach a fingerprint for an unverified
/// peer through this function: [`Session::receive_accept`] only ever computes one from a
/// [`HandshakeResponse`] this call already accepted.
pub fn complete_handshake(
    identity: &Identity,
    pending: &PendingOffer,
    response: &HandshakeResponse,
    rng: &mut impl RandomSource,
) -> Result<Ratchet, Error> {
    let signed = concat(&[&response.ik, &response.ek]);
    verify_signature(&response.sik, &signed, &response.sig)?;

    let sk = x3dh_secret_initiator(
        &pending.spk_secret,
        &identity.secret.agreement,
        &pending.opk_secret,
        &response.ik,
        &response.ek,
    )?;

    let mut ratchet = Ratchet::init_as_initiator(*sk, *pending.spk_secret, pending.bundle.spk);
    ratchet.decrypt(&response.boot, rng)?;
    Ok(ratchet)
}

// ---------------------------------------------------------------------------------------------
// The Double Ratchet
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
struct SkippedKeys {
    by_id: BTreeMap<([u8; 32], u32), Zeroizing<[u8; 32]>>,
    order: VecDeque<([u8; 32], u32)>,
}

impl SkippedKeys {
    fn new() -> Self {
        Self {
            by_id: BTreeMap::new(),
            order: VecDeque::new(),
        }
    }

    fn insert(&mut self, dh: [u8; 32], n: u32, key: Zeroizing<[u8; 32]>) {
        let id = (dh, n);
        if self.by_id.insert(id, key).is_none() {
            self.order.push_back(id);
        }
        while self.order.len() > MAX_SKIPPED_KEYS {
            if let Some(oldest) = self.order.pop_front() {
                self.by_id.remove(&oldest);
            }
        }
    }

    fn take(&mut self, dh: [u8; 32], n: u32) -> Option<Zeroizing<[u8; 32]>> {
        let id = (dh, n);
        let key = self.by_id.remove(&id)?;
        self.order.retain(|entry| *entry != id);
        Some(key)
    }
}

/// One party's half of a Double Ratchet session: a sending chain, a receiving chain, and the
/// skipped-key store that lets messages arrive out of order.
#[derive(Clone)]
pub struct Ratchet {
    root_key: Zeroizing<[u8; 32]>,
    dhs_secret: Zeroizing<[u8; 32]>,
    dhs_public: [u8; 32],
    dhr: Option<[u8; 32]>,
    send_chain: Option<Zeroizing<[u8; 32]>>,
    recv_chain: Option<Zeroizing<[u8; 32]>>,
    n_send: u32,
    n_recv: u32,
    prev_chain_len: u32,
    skipped: SkippedKeys,
}

impl Ratchet {
    /// Open a ratchet as the responder (wire sense): derive a sending chain immediately against
    /// the initiator's retained signed-prekey, with no receiving chain yet.
    fn init_as_responder(
        root_key: [u8; 32],
        own_ek_secret: [u8; 32],
        own_ek_public: [u8; 32],
        their_spk_public: [u8; 32],
    ) -> Result<Self, Error> {
        let dh_out = diffie_hellman_raw(&own_ek_secret, &their_spk_public)?;
        let (new_root, send_chain) = kdf_root(&root_key, &dh_out)?;
        Ok(Self {
            root_key: Zeroizing::new(new_root),
            dhs_secret: Zeroizing::new(own_ek_secret),
            dhs_public: own_ek_public,
            dhr: Some(their_spk_public),
            send_chain: Some(Zeroizing::new(send_chain)),
            recv_chain: None,
            n_send: 0,
            n_recv: 0,
            prev_chain_len: 0,
            skipped: SkippedKeys::new(),
        })
    }

    /// Open a ratchet as the initiator (wire sense): reuse the retained signed-prekey as the
    /// first ratchet keypair, with no peer ratchet key and no chain yet. The first inbound
    /// message (`boot`) supplies the peer's key and completes the receiving chain.
    fn init_as_initiator(
        root_key: [u8; 32],
        own_dhs_secret: [u8; 32],
        own_dhs_public: [u8; 32],
    ) -> Self {
        Self {
            root_key: Zeroizing::new(root_key),
            dhs_secret: Zeroizing::new(own_dhs_secret),
            dhs_public: own_dhs_public,
            dhr: None,
            send_chain: None,
            recv_chain: None,
            n_send: 0,
            n_recv: 0,
            prev_chain_len: 0,
            skipped: SkippedKeys::new(),
        }
    }

    /// Encrypt `plaintext`, advancing the sending chain by one step.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<RatchetMessage, Error> {
        let Some(chain) = self.send_chain.clone() else {
            return Err(Error::NoChain);
        };
        let (message_key, next_chain) = kdf_chain(&chain)?;
        self.send_chain = Some(Zeroizing::new(next_chain));

        let dh = self.dhs_public;
        let pn = self.prev_chain_len;
        let n = self.n_send;
        self.n_send = self.n_send.checked_add(1).ok_or(Error::CounterOverflow)?;

        let padded = pad(plaintext);
        let aad = header_aad(&dh, pn, n);
        let ct = aead_encrypt(&message_key, &aad, &padded)?;
        Ok(RatchetMessage { dh, pn, n, ct })
    }

    /// Decrypt `msg`, advancing the receiving chain (and performing a DH ratchet step, when the
    /// header names a new peer key) only once the AEAD tag verifies.
    ///
    /// The whole state is cloned, mutated on the clone, and only committed back on success, so a
    /// forged frame with a plausible header but garbage ciphertext cannot burn a skipped-key slot
    /// or desync the receiving chain.
    pub fn decrypt(
        &mut self,
        msg: &RatchetMessage,
        rng: &mut impl RandomSource,
    ) -> Result<Vec<u8>, Error> {
        let mut trial = self.clone();

        if let Some(message_key) = trial.skipped.take(msg.dh, msg.n) {
            let plaintext = decrypt_with_key(&message_key, msg)?;
            *self = trial;
            return Ok(plaintext);
        }

        if trial.dhr != Some(msg.dh) {
            trial.skip_current_receiving_chain(msg.pn)?;
            trial.dh_ratchet_receive(msg.dh, rng)?;
        }
        trial.skip_current_receiving_chain(msg.n)?;

        let Some(chain) = trial.recv_chain.clone() else {
            return Err(Error::NoChain);
        };
        let (message_key, next_chain) = kdf_chain(&chain)?;
        trial.recv_chain = Some(Zeroizing::new(next_chain));
        trial.n_recv = trial.n_recv.checked_add(1).ok_or(Error::CounterOverflow)?;

        let plaintext = decrypt_with_key(&Zeroizing::new(message_key), msg)?;
        *self = trial;
        Ok(plaintext)
    }

    /// Advance the current receiving chain up to (but not including) counter `until`, storing
    /// each skipped message key. A no-op when there is no receiving chain yet, or `until` is
    /// already behind the current position.
    fn skip_current_receiving_chain(&mut self, until: u32) -> Result<(), Error> {
        let Some(dhr) = self.dhr else { return Ok(()) };
        let Some(mut chain) = self.recv_chain.take() else {
            return Ok(());
        };
        if until <= self.n_recv {
            self.recv_chain = Some(chain);
            return Ok(());
        }
        let span = until - self.n_recv;
        if span > MAX_SKIP {
            self.recv_chain = Some(chain);
            return Err(Error::TooManySkipped);
        }
        for _ in 0..span {
            let (message_key, next_chain) = kdf_chain(&chain)?;
            self.skipped
                .insert(dhr, self.n_recv, Zeroizing::new(message_key));
            chain = Zeroizing::new(next_chain);
            self.n_recv = self.n_recv.checked_add(1).ok_or(Error::CounterOverflow)?;
        }
        self.recv_chain = Some(chain);
        Ok(())
    }

    /// A DH ratchet step: adopt the peer's new ratchet key, derive a fresh receiving chain
    /// against it with the current keypair, then generate a fresh own keypair and derive a
    /// fresh sending chain against the same peer key.
    fn dh_ratchet_receive(
        &mut self,
        new_dhr: [u8; 32],
        rng: &mut impl RandomSource,
    ) -> Result<(), Error> {
        self.prev_chain_len = self.n_send;
        self.n_send = 0;
        self.n_recv = 0;
        self.dhr = Some(new_dhr);

        let dh_out = diffie_hellman_raw(&self.dhs_secret, &new_dhr)?;
        let (root_after_recv, recv_chain) = kdf_root(&self.root_key, &dh_out)?;
        self.root_key = Zeroizing::new(root_after_recv);
        self.recv_chain = Some(Zeroizing::new(recv_chain));

        let (dhs_secret, dhs_public) = generate_x25519_keypair(rng);
        self.dhs_secret = Zeroizing::new(dhs_secret);
        self.dhs_public = dhs_public;

        let dh_out2 = diffie_hellman_raw(&self.dhs_secret, &new_dhr)?;
        let (root_after_send, send_chain) = kdf_root(&self.root_key, &dh_out2)?;
        self.root_key = Zeroizing::new(root_after_send);
        self.send_chain = Some(Zeroizing::new(send_chain));
        Ok(())
    }
}

fn decrypt_with_key(message_key: &[u8; 32], msg: &RatchetMessage) -> Result<Vec<u8>, Error> {
    let aad = header_aad(&msg.dh, msg.pn, msg.n);
    let padded = aead_decrypt(message_key, &aad, &msg.ct)?;
    unpad(&padded)
}

fn kdf_root(root_key: &[u8; 32], dh_out: &[u8; 32]) -> Result<([u8; 32], [u8; 32]), Error> {
    let mut okm = [0u8; 64];
    hkdf_sha256(root_key, dh_out, ROOT_INFO, &mut okm)?;
    let (root_half, chain_half) = okm.split_at(32);
    let new_root: [u8; 32] = root_half.try_into().map_err(|_| Error::Internal)?;
    let new_chain: [u8; 32] = chain_half.try_into().map_err(|_| Error::Internal)?;
    Ok((new_root, new_chain))
}

fn kdf_chain(chain_key: &[u8; 32]) -> Result<([u8; 32], [u8; 32]), Error> {
    let message_key = hmac_sha256(chain_key, &[0x01])?;
    let next_chain = hmac_sha256(chain_key, &[0x02])?;
    Ok((message_key, next_chain))
}

fn message_aead_params(message_key: &[u8; 32]) -> Result<(Key, XNonce), Error> {
    let mut key_bytes = [0u8; 32];
    hkdf_sha256(&[0u8; 32], message_key, MESSAGE_KEY_INFO, &mut key_bytes)?;
    let mut nonce_bytes = [0u8; 24];
    hkdf_sha256(&[0u8; 32], message_key, NONCE_INFO, &mut nonce_bytes)?;
    Ok((Key::from(key_bytes), XNonce::from(nonce_bytes)))
}

fn aead_encrypt(message_key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    let (key, nonce) = message_aead_params(message_key)?;
    XChaCha20Poly1305::new(&key)
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| Error::Aead)
}

fn aead_decrypt(message_key: &[u8; 32], aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
    let (key, nonce) = message_aead_params(message_key)?;
    XChaCha20Poly1305::new(&key)
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| Error::Aead)
}

fn header_aad(dh: &[u8; 32], pn: u32, n: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(40);
    aad.extend_from_slice(dh);
    aad.extend_from_slice(&pn.to_be_bytes());
    aad.extend_from_slice(&n.to_be_bytes());
    aad
}

fn pad(plaintext: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(plaintext.len() + PAD_BLOCK);
    out.extend_from_slice(plaintext);
    out.push(0x80);
    let remainder = out.len() % PAD_BLOCK;
    if remainder != 0 {
        out.resize(out.len() + (PAD_BLOCK - remainder), 0);
    }
    out
}

fn unpad(padded: &[u8]) -> Result<Vec<u8>, Error> {
    let marker = padded
        .iter()
        .rposition(|&byte| byte != 0)
        .ok_or(Error::Padding)?;
    if padded.get(marker) != Some(&0x80) {
        return Err(Error::Padding);
    }
    padded
        .get(..marker)
        .map(<[u8]>::to_vec)
        .ok_or(Error::Padding)
}

// ---------------------------------------------------------------------------------------------
// Session state machine
// ---------------------------------------------------------------------------------------------

/// Which side of the handshake a session played, once negotiation has started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// This side sent `init`.
    Initiator,
    /// This side sent `accept`.
    Responder,
}

/// How far a conversation has got towards being encrypted.
///
/// Only [`SessionState::Established`] carries a ratchet that will decrypt content, which is what
/// makes "an accept does not mean established" a property of the type.
///
/// Deliberately not `Debug`: two of these variants hold key material, and a derived formatter would
/// put it in whatever log the caller happens to write.
pub enum SessionState {
    /// Nothing has been offered yet.
    Idle,
    /// We sent `init` and are waiting for an answer.
    Offered {
        /// The keys we offered, kept until the answer arrives.
        pending: PendingOffer,
    },
    /// They sent `init` and we have not answered yet.
    OfferReceived {
        /// What they offered.
        bundle: PreKeyBundle,
    },
    /// We answered and are waiting for the `ack` that proves they can decrypt.
    ///
    /// An `accept` can be lost, so showing a lock here would tell the user something untrue. This
    /// state deliberately exposes no way to decrypt content.
    AwaitingAck {
        /// The ratchet, held but not yet trusted to carry content.
        ratchet: Ratchet,
    },
    /// The session works in both directions.
    Established {
        /// The live ratchet.
        ratchet: Ratchet,
    },
    /// They refused.
    Rejected,
    /// The session ended.
    Closed,
}

/// One pairwise end-to-end encrypted conversation.
///
/// An `accept` alone never reaches [`SessionState::Established`]: the responder side lands in
/// [`SessionState::AwaitingAck`], which exposes no way to decrypt content, and only
/// [`Session::receive_ack`] can move it onward. There is no method on this type that decrypts a
/// `msg`/`media` payload from any other state, which is what makes the "accept is not
/// established" rule a property of the API rather than a rule callers must remember to enforce.
pub struct Session {
    state: SessionState,
    trust: PeerTrust,
    role: Option<Role>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            state: SessionState::Idle,
            trust: PeerTrust::new(),
            role: None,
        }
    }
}

impl Session {
    /// A session that has not started negotiating with anyone.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start as the wire's initiator: publish a fresh prekey bundle.
    pub fn start(
        &mut self,
        identity: &Identity,
        rng: &mut impl RandomSource,
    ) -> Result<PreKeyBundle, Error> {
        let pending = create_offer(identity, rng)?;
        let bundle = pending.bundle.clone();
        self.state = SessionState::Offered { pending };
        Ok(bundle)
    }

    /// Record an inbound `init`, returning the offered (not yet verified or pinned) fingerprint
    /// for a pre-acceptance prompt.
    ///
    /// When this side already has its own offer outstanding, the caller must resolve the
    /// crossing-offer tiebreak with [`keeps_own_offer`] before calling this: a side that keeps
    /// its own offer must not overwrite it by recording the peer's.
    pub fn receive_offer(&mut self, bundle: PreKeyBundle) -> Fingerprint {
        let offered = Fingerprint::of_signing_key(&bundle.sik);
        self.state = SessionState::OfferReceived { bundle };
        offered
    }

    /// Accept a recorded offer: verify it, derive the session secret, and answer with a
    /// `HandshakeResponse`. Lands in [`SessionState::AwaitingAck`], not established.
    pub fn accept(
        &mut self,
        identity: &Identity,
        rng: &mut impl RandomSource,
    ) -> Result<HandshakeResponse, Error> {
        let SessionState::OfferReceived { bundle } = &self.state else {
            return Err(Error::WrongState);
        };
        let (response, ratchet) = accept_offer(identity, bundle, rng)?;

        let fingerprint = Fingerprint::of_signing_key(&bundle.sik);
        if let PinOutcome::Changed { previous } = self.trust.observe(fingerprint) {
            return Err(Error::FingerprintChanged {
                previous,
                current: fingerprint,
            });
        }

        self.role = Some(Role::Responder);
        self.state = SessionState::AwaitingAck { ratchet };
        Ok(response)
    }

    /// Reject a recorded offer.
    pub fn reject(&mut self, reason: Option<String>) -> Frame {
        self.state = SessionState::Rejected;
        Frame::Reject { reason }
    }

    /// Record an inbound `reject` for an offer this side sent.
    pub fn receive_reject(&mut self) {
        self.state = SessionState::Rejected;
    }

    /// Complete the handshake as the initiator: verify the responder's signature before pinning
    /// its fingerprint, derive the session secret, and decrypt `boot`. Reaches
    /// [`SessionState::Established`] directly, since the initiator has no further frame to wait
    /// for.
    pub fn receive_accept(
        &mut self,
        identity: &Identity,
        response: &HandshakeResponse,
        rng: &mut impl RandomSource,
    ) -> Result<(), Error> {
        let SessionState::Offered { pending } = &self.state else {
            return Err(Error::WrongState);
        };
        let ratchet = complete_handshake(identity, pending, response, rng)?;

        let fingerprint = Fingerprint::of_signing_key(&response.sik);
        if let PinOutcome::Changed { previous } = self.trust.observe(fingerprint) {
            return Err(Error::FingerprintChanged {
                previous,
                current: fingerprint,
            });
        }

        self.role = Some(Role::Initiator);
        self.state = SessionState::Established { ratchet };
        Ok(())
    }

    /// Produce the initiator's `ack`: an empty-plaintext ratchet message proving the session
    /// works.
    pub fn make_ack(&mut self) -> Result<RatchetMessage, Error> {
        self.send(&[])
    }

    /// Decrypt the initiator's `ack`. Only this call moves a responder from
    /// [`SessionState::AwaitingAck`] to [`SessionState::Established`]; a lost or not-yet-arrived
    /// `ack` leaves the session exactly where it was, never showing established early.
    pub fn receive_ack(
        &mut self,
        ct: &RatchetMessage,
        rng: &mut impl RandomSource,
    ) -> Result<(), Error> {
        let SessionState::AwaitingAck { ratchet } = &mut self.state else {
            return Err(Error::WrongState);
        };
        ratchet.decrypt(ct, rng)?;
        let SessionState::AwaitingAck { ratchet } =
            mem::replace(&mut self.state, SessionState::Idle)
        else {
            return Err(Error::WrongState);
        };
        self.state = SessionState::Established { ratchet };
        Ok(())
    }

    /// Encrypt a `msg`/`media` payload. Only available once established.
    pub fn send(&mut self, plaintext: &[u8]) -> Result<RatchetMessage, Error> {
        let SessionState::Established { ratchet } = &mut self.state else {
            return Err(Error::WrongState);
        };
        ratchet.encrypt(plaintext)
    }

    /// Decrypt a `msg`/`media` payload. Only available once established: there is no state from
    /// which this call can reach a receiving chain before then.
    pub fn receive(
        &mut self,
        ct: &RatchetMessage,
        rng: &mut impl RandomSource,
    ) -> Result<Vec<u8>, Error> {
        let SessionState::Established { ratchet } = &mut self.state else {
            return Err(Error::WrongState);
        };
        ratchet.decrypt(ct, rng)
    }

    /// End the session locally.
    pub fn close(&mut self) -> Frame {
        self.state = SessionState::Closed;
        Frame::Close
    }

    /// Record an inbound `close`.
    pub fn receive_close(&mut self) {
        self.state = SessionState::Closed;
    }

    /// Whether this session can currently send or receive content.
    pub const fn is_established(&self) -> bool {
        matches!(self.state, SessionState::Established { .. })
    }

    /// The peer's pinned fingerprint, once one has been observed.
    pub fn peer_fingerprint(&self) -> Option<Fingerprint> {
        self.trust.pinned()
    }

    /// Whether the pinned fingerprint has been confirmed out of band.
    pub fn is_peer_verified(&self) -> bool {
        self.trust.is_verified()
    }

    /// Mark the pinned fingerprint as confirmed out of band.
    pub fn mark_peer_verified(&mut self) {
        self.trust.set_verified(true);
    }

    /// Accept a fingerprint change the caller has explicitly decided to trust, so the operation
    /// that reported [`Error::FingerprintChanged`] can be retried and will succeed this time.
    pub fn confirm_fingerprint_change(&mut self, fingerprint: Fingerprint) {
        self.trust.repin(fingerprint);
    }

    /// Which side of the handshake this session played, once negotiation has started.
    pub const fn role(&self) -> Option<Role> {
        self.role
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRng(u64);

    impl TestRng {
        fn seeded(seed: u64) -> Self {
            Self(seed)
        }
    }

    impl RandomSource for TestRng {
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for chunk in dest.chunks_mut(8) {
                self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = self.0;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                z ^= z >> 31;
                let bytes = z.to_le_bytes();
                chunk.copy_from_slice(&bytes[..chunk.len()]);
            }
        }
    }

    struct Pair {
        alice_identity: Identity,
        bob_identity: Identity,
        alice: Session,
        bob: Session,
    }

    fn establish() -> Pair {
        let mut rng = TestRng::seeded(1);
        let alice_identity = Identity::generate(&mut rng);
        let bob_identity = Identity::generate(&mut rng);
        let mut alice = Session::new();
        let mut bob = Session::new();

        let bundle = alice.start(&alice_identity, &mut rng).unwrap();
        bob.receive_offer(bundle);
        let response = bob.accept(&bob_identity, &mut rng).unwrap();
        assert!(!bob.is_established());

        alice
            .receive_accept(&alice_identity, &response, &mut rng)
            .unwrap();
        assert!(alice.is_established());

        let ack = alice.make_ack().unwrap();
        bob.receive_ack(&ack, &mut rng).unwrap();
        assert!(bob.is_established());

        Pair {
            alice_identity,
            bob_identity,
            alice,
            bob,
        }
    }

    #[test]
    fn full_handshake_establishes_both_sides() {
        let pair = establish();
        assert_eq!(pair.alice.role(), Some(Role::Initiator));
        assert_eq!(pair.bob.role(), Some(Role::Responder));
        assert_eq!(
            pair.alice.peer_fingerprint(),
            Some(pair.bob_identity.fingerprint())
        );
        assert_eq!(
            pair.bob.peer_fingerprint(),
            Some(pair.alice_identity.fingerprint())
        );
    }

    #[test]
    fn accept_alone_does_not_establish() {
        let mut rng = TestRng::seeded(2);
        let alice_identity = Identity::generate(&mut rng);
        let bob_identity = Identity::generate(&mut rng);
        let mut alice = Session::new();
        let mut bob = Session::new();

        let bundle = alice.start(&alice_identity, &mut rng).unwrap();
        bob.receive_offer(bundle);
        bob.accept(&bob_identity, &mut rng).unwrap();

        assert!(!bob.is_established());
        assert_eq!(bob.send(b"hello"), Err(Error::WrongState));
    }

    #[test]
    fn established_session_encrypts_and_decrypts() {
        let mut pair = establish();
        let mut rng = TestRng::seeded(3);

        let ct = pair.alice.send(b"hello bob").unwrap();
        let plaintext = pair.bob.receive(&ct, &mut rng).unwrap();
        assert_eq!(plaintext, b"hello bob");

        let ct = pair.bob.send(b"hello alice").unwrap();
        let plaintext = pair.alice.receive(&ct, &mut rng).unwrap();
        assert_eq!(plaintext, b"hello alice");
    }

    #[test]
    fn out_of_order_delivery_still_decrypts() {
        let mut pair = establish();
        let mut rng = TestRng::seeded(4);

        let first = pair.alice.send(b"one").unwrap();
        let second = pair.alice.send(b"two").unwrap();
        let third = pair.alice.send(b"three").unwrap();

        assert_eq!(pair.bob.receive(&third, &mut rng).unwrap(), b"three");
        assert_eq!(pair.bob.receive(&first, &mut rng).unwrap(), b"one");
        assert_eq!(pair.bob.receive(&second, &mut rng).unwrap(), b"two");
    }

    #[test]
    fn skip_bound_is_enforced() {
        let mut pair = establish();
        let mut rng = TestRng::seeded(5);

        let mut far = pair.alice.send(b"far").unwrap();
        far.n += MAX_SKIP + 1;
        assert_eq!(pair.bob.receive(&far, &mut rng), Err(Error::TooManySkipped));
    }

    #[test]
    fn tampered_ciphertext_fails_to_decrypt() {
        let mut pair = establish();
        let mut rng = TestRng::seeded(6);

        let mut ct = pair.alice.send(b"hello").unwrap();
        let last = ct.ct.len() - 1;
        if let Some(byte) = ct.ct.get_mut(last) {
            *byte ^= 0xFF;
        }
        assert_eq!(pair.bob.receive(&ct, &mut rng), Err(Error::Aead));
    }

    #[test]
    fn forged_responder_signature_is_rejected() {
        let mut rng = TestRng::seeded(7);
        let alice_identity = Identity::generate(&mut rng);
        let bob_identity = Identity::generate(&mut rng);
        let mallory_identity = Identity::generate(&mut rng);
        let mut alice = Session::new();
        let mut bob = Session::new();

        let bundle = alice.start(&alice_identity, &mut rng).unwrap();
        bob.receive_offer(bundle);
        let mut response = bob.accept(&bob_identity, &mut rng).unwrap();

        // splice in an unrelated identity's signing key, simulating an attacker presenting a
        // real user's fingerprint over its own session
        response.sik = mallory_identity.public().signing;

        let result = alice.receive_accept(&alice_identity, &response, &mut rng);
        assert_eq!(result, Err(Error::InvalidSignature));
        assert!(!alice.is_established());
        assert_eq!(alice.peer_fingerprint(), None);
    }

    #[test]
    fn crossing_offers_lower_fingerprint_wins() {
        let mut rng = TestRng::seeded(8);
        let a = Identity::generate(&mut rng).fingerprint();
        let b = Identity::generate(&mut rng).fingerprint();
        let (lower, higher) = if a < b { (a, b) } else { (b, a) };

        assert!(keeps_own_offer(lower, Some(higher)));
        assert!(!keeps_own_offer(higher, Some(lower)));
        assert!(!keeps_own_offer(lower, None));
    }

    #[test]
    fn changed_fingerprint_is_refused() {
        let mut rng = TestRng::seeded(9);
        let alice_identity = Identity::generate(&mut rng);
        let bob_identity = Identity::generate(&mut rng);
        let mallory_identity = Identity::generate(&mut rng);

        let mut alice = Session::new();
        alice.trust.observe(mallory_identity.fingerprint());

        let mut bob = Session::new();
        let bundle = alice.start(&alice_identity, &mut rng).unwrap();
        bob.receive_offer(bundle);
        let response = bob.accept(&bob_identity, &mut rng).unwrap();

        let result = alice.receive_accept(&alice_identity, &response, &mut rng);
        assert_eq!(
            result,
            Err(Error::FingerprintChanged {
                previous: mallory_identity.fingerprint(),
                current: bob_identity.fingerprint(),
            })
        );
        assert!(!alice.is_established());

        alice.confirm_fingerprint_change(bob_identity.fingerprint());
        alice
            .receive_accept(&alice_identity, &response, &mut rng)
            .unwrap();
        assert!(alice.is_established());
    }

    #[test]
    fn safety_number_is_eight_groups_of_four() {
        let mut rng = TestRng::seeded(10);
        let fingerprint = Identity::generate(&mut rng).fingerprint();
        let rendered = fingerprint.safety_number();
        let groups: Vec<&str> = rendered.split(' ').collect();
        assert_eq!(groups.len(), 8);
        for group in groups {
            assert_eq!(group.len(), 4);
            assert!(
                group
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase())
            );
        }
    }

    #[test]
    fn frag_reassembles_in_order_and_rejects_a_gap() {
        let whole = b"hello obby world".to_vec();
        let (first_half, second_half) = whole.split_at(8);
        let fragments = alloc::vec![
            Frag {
                id: "abc".into(),
                i: 0,
                n: 2,
                ct: first_half.to_vec(),
            },
            Frag {
                id: "abc".into(),
                i: 1,
                n: 2,
                ct: second_half.to_vec(),
            },
        ];
        assert_eq!(reassemble(&fragments).unwrap(), whole);

        let missing_second = &fragments[..1];
        assert_eq!(reassemble(missing_second), Err(Error::Fragmentation));
    }

    #[test]
    fn reject_and_close_transition_state_and_frame() {
        let mut rng = TestRng::seeded(11);
        let alice_identity = Identity::generate(&mut rng);
        let mut alice = Session::new();
        let mut bob = Session::new();

        let bundle = alice.start(&alice_identity, &mut rng).unwrap();
        let offered = bob.receive_offer(bundle.clone());
        assert_eq!(offered, alice_identity.fingerprint());

        let frame = bob.reject(Some("busy".into()));
        assert_eq!(
            frame,
            Frame::Reject {
                reason: Some("busy".into())
            }
        );
        assert!(!bob.is_established());
        alice.receive_reject();
        assert_eq!(alice.send(b"too late"), Err(Error::WrongState));

        let mut pair = establish();
        let frame = pair.alice.close();
        assert_eq!(frame, Frame::Close);
        pair.bob.receive_close();
        assert_eq!(pair.alice.send(b"too late"), Err(Error::WrongState));
        assert_eq!(pair.bob.send(b"too late"), Err(Error::WrongState));

        // the frame set also carries plain content and handshake frames under their own names
        let init = Frame::Init {
            bundle,
            account: Some("alice".into()),
        };
        assert!(matches!(init, Frame::Init { .. }));
        assert_eq!(PROTOCOL_VERSION, 1);
    }

    #[test]
    fn frame_wraps_every_content_and_handshake_variant() {
        let mut pair = establish();
        let ct = pair.alice.send(b"hi").unwrap();
        let msg = Frame::Msg { ct: ct.clone() };
        let media = Frame::Media { ct: ct.clone() };
        let ack = Frame::Ack { ct };
        assert!(matches!(msg, Frame::Msg { .. }));
        assert!(matches!(media, Frame::Media { .. }));
        assert!(matches!(ack, Frame::Ack { .. }));

        let mut rng = TestRng::seeded(12);
        let alice_identity = Identity::generate(&mut rng);
        let bob_identity = Identity::generate(&mut rng);
        let mut alice = Session::new();
        let mut bob = Session::new();
        let bundle = alice.start(&alice_identity, &mut rng).unwrap();
        bob.receive_offer(bundle);
        let response = bob.accept(&bob_identity, &mut rng).unwrap();
        let accept = Frame::Accept {
            response,
            account: None,
        };
        assert!(matches!(accept, Frame::Accept { .. }));
    }

    #[test]
    fn peer_verification_tracks_out_of_band_confirmation() {
        let mut pair = establish();
        assert!(!pair.alice.is_peer_verified());
        pair.alice.mark_peer_verified();
        assert!(pair.alice.is_peer_verified());
        assert!(!pair.bob.is_peer_verified());
    }
}
