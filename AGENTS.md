# obby-client: agent guide

The protocol and state engine behind every Obby client. One Rust core, bound into C, TypeScript
(browser and Bun), Python, and Dart, so a new Obby client is a UI and nothing else.

It does no I/O. The host owns the socket, the clock and the screen; the engine owns the protocol and
the model.

## Layout

```
crates/
  obby-proto/          wire format, no state, no connection
    src/message.rs     line parsing and serialisation
    src/tags.rs        message tags and their escaping
    src/casemap.rs     CASEMAPPING and the CaseFolded key type
    src/isupport.rs    005 tokens and the typed settings they drive
    src/mode.rs        MODE parsing, arity driven by CHANMODES and PREFIX
    src/format.rs      mIRC formatting and CTCP into spans
    src/servertime.rs  the server-time tag into milliseconds
  tests/               the official ircdocs parser vectors
  obby-client/         the engine
    src/client.rs      the connection: registration, PING, ISUPPORT, the poll loop
    src/caps.rs        capability negotiation state
    src/sasl.rs        mechanisms, response chunking, the exchange state
    src/scram.rs       SCRAM-SHA-256, so the password never crosses the wire
    src/model.rs       channels, members, conversations, the message log
    src/session.rs     protocol lines folded into the model
    src/batch.rs       batch tracking, nesting, in-batch dedup
    src/timer.rs       deadlines and reconnect backoff, no clock of its own
    src/label.rs       labeled-response correlation
    src/command.rs     what a host asks the connection to do
    src/monitor.rs     watching who is online
    src/extensions.rs  the Obby extensions: link previews, command list, invitations, bots
    src/voice.rs       voice signalling and room state, never the media plane
    src/e2ee.rs        X3DH and the double ratchet
  tests/transcripts/   recorded sessions, replayed and snapshotted
bindings/
  obby-ffi/            the C ABI, and the only place unsafe lives
  obby-wasm/           one wasm-pack artifact for the browser and for Bun
  obby-python/         PyO3, packaged by maturin
  obby-dart/           a Dart package over the C ABI, no second Rust crate
```

## Commands

```
make check     # after every change
make test      # nextest plus doctests
make fix       # autofix formatting and the mechanical lints
make ci        # everything CI runs, before a commit
```

`make help` lists the rest. The Makefile is the only source of truth for what CI runs; every CI job
body is one `make` target.

A changed snapshot is a behaviour change. Read the diff with `make snap`, and only then
`make snap-accept`. Never accept one blindly and never edit a `.snap` by hand.

## Rules

- Work is done when `make ci` passes. Not before.
- No `unwrap`, `expect`, `panic!`, `todo!` or `unimplemented!` outside tests. Clippy denies them.
- No `#[allow(...)]` to silence a denied lint. `allow_attributes` is denied for exactly this reason:
  fix the code.
- Comments explain why, never what, and never narrate a change.
- A new dependency needs a reason. The core's dependency list is a portability budget: everything in
  it has to build for `wasm32-unknown-unknown` and for `no_std`.
- Commit messages are one line.

## Architecture and decisions

**Sans-io, synchronous, poll-driven.** Bytes and time go in through `handle_bytes` and `tick`; bytes,
events and the next wake-up come out through `poll_transmit`, `poll_event` and `poll_timeout`. This
is the `quinn-proto` and `rustls` shape. Callbacks are deliberately absent: a callback needs a
different lifetime, threading and re-entrancy contract in each of the five languages this is bound
into, and a drain loop needs none. It also makes every test a pure function of bytes in to events
out, with no runtime and no mocks.

**Effects are queued, never re-entrant.** A handler mutates state and pushes onto the outbound
queues. It never calls back into the engine mid-update, so firing an event can never corrupt the
update that produced it.

**Owned types, not borrowed.** `Message` owns its strings. A borrowed `Message<'a>` parses faster,
but every consumer reaches it across a boundary where a Rust lifetime cannot follow, so the copy has
to happen somewhere. Here it happens once.

**Time is a `u64` of host-supplied milliseconds**, not `Instant`. `Instant` is not `no_std`, does not
exist in wasm, and cannot cross an FFI boundary. The host already has a clock.

**A person is described once.** Nick, account, away state and metadata live on `Person`, keyed by
the folded nick, not copied into a `Membership` per channel. A `Membership` holds only what is
channel-specific, which is the prefixes. One record for one person is what keeps two views of them
from disagreeing.

**The core owns the model, including the messages.** Not just protocol state: channels, members,
private chats, metadata, unread counts and the message lists. Retention is one configurable limit per
target, applied identically to live traffic and to history backfill. Deduplicating a replay and
merging a history page both need to see what is already held, so a forgetful core would push its
hardest logic into every app. Persistence is still the app's: the core offers snapshot and restore.

**Casemapping is a type, not a convention.** A nick or channel is only ever a map key as
`CaseFolded`, produced by the connection's `Casemapping`. `to_lowercase` is wrong on any server that
does not advertise `ascii`, which is most of them: `rfc1459` folds `{}|^` together with `[]\~`, and
getting that wrong gives one person two records.

**The trailing `:` is written only where it is needed** (empty parameter, one containing a space, or
one starting with a colon). Both forms are legal and parse identically; this one round-trips.

**Mode arity comes from the server, never a table.** `parse_channel_modes` reads `CHANMODES` and
`PREFIX` to decide which letters consume an argument. Getting one wrong shifts every argument after
it onto the wrong letter, so an unadvertised letter is treated as taking none: a missing argument is
the cheaper mistake than a shifted one.

**Reconnection is the engine's, dialling is the host's.** The `Client` survives a dead link with its
state intact and says when to retry; the host opens the socket. The replay after `handle_connected`
authenticates, rejoins with the stored channel key, resubscribes MONITOR and metadata, and asks for
history from the last message it saw.

**Nothing returns a `Result` during normal operation.** An unparseable line is counted and dropped.
Everything else is a typed event.

**The TypeScript definitions are generated, never written.** `make ts-types` derives them from the
Rust with `ts-rs`, CI fails on drift, `make ts-check` type-checks a consumer against them and then
runs that consumer against the built module. That last step exists because `tsc` alone only checks
the generated file against itself: it cannot see that `serde_wasm_bindgen` hands JavaScript a `Map`
where the definition says object.

**Everything that crosses a binding serialises `snake_case`.** `Command`, `Event` and `Change` all
have a `type` tag in `snake_case`, so a Python, Dart or JavaScript host reads one convention rather
than guessing per type which side of the boundary chose the casing.

**Every line reaches the host.** Anything the engine does not model yet is emitted as `Event::Raw`,
so a host is never blind to traffic and can implement ahead of the core.

**A handle is not synchronised, and that is a documented contract, not an accident.** Every
`obby-ffi` function turns the raw pointer into an exclusive `&mut Client`, so two threads touching
one handle is undefined behaviour rather than a race with a wrong answer. C cannot enforce this, so
it is stated in the crate docs, in the generated header, on every function that takes a handle, and
in the Dart wrapper. Do not remove those.

**`obby-ffi` is the only crate where `unsafe` is allowed.** Every other crate denies it, and that
crate exists so they can: the C boundary cannot be written without it. Every function there has a
`# Safety` section, and nothing may panic across the boundary, because a panic unwinding into C is
undefined behaviour.

**Feature flags are a portability budget**, not a menu: `std`, `serde`, `obby`, `voice`, `e2ee`. A
stream bridge takes none of the chat model; the web app takes all of it. `voice` is signaling and
room state only, never the media plane. `e2ee` is fully portable, because X25519, Ed25519,
XChaCha20-Poly1305 and HKDF-SHA256 all have pure-Rust implementations that build for wasm.

## Testing

Three layers. Unit tests next to the code. The official ircdocs parser vectors in
`crates/obby-proto/tests`, generated from upstream YAML and never edited by hand. Golden transcripts
in `crates/obby-client/tests/transcripts`, replayed into a snapshot of everything the engine sent
and the model it ended up with.

`make live` runs the engine against `irc.h4ks.com:6697`. It is ignored by default, because a suite
that fails when the network drops is a suite people learn to ignore. It is the only test here that
proves the engine against a server whose answers we did not write down first, so run it before a
release: it asserts zero unparseable lines. Golden transcripts are written from the specifications
first, then captured from a real session, and replayed byte for byte.

## Sources

The protocol this implements lives in three places, all of them outside this repo:

- IRCv3 specifications, <https://ircv3.net/irc/>, and the Modern IRC document,
  <https://modern.ircdocs.horse/>.
- The Obby extensions, <https://github.com/obbyworld/extensions>. Treat it as background, not as a
  specification: it is incomplete and in places wrong. It documents a batch type and an attribution
  tag for channel-bots that do not exist on the wire, and a `manage-bots` permission that exists in
  neither the server nor the client, while the voice signalling it describes covers about half the
  frame types actually in use. Whole subsystems appear nowhere in it. Implement against the wire.
- The client this replaces the protocol half of, <https://github.com/obbyworld/obby>, and the
  server, <https://github.com/obbyworld/ObbyIRCd>.
