# obby-client

[![CI](https://github.com/obbyworld/obby-client/actions/workflows/ci.yml/badge.svg)](https://github.com/obbyworld/obby-client/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/obby-client?logo=rust)](https://crates.io/crates/obby-client)
[![npm](https://img.shields.io/npm/v/obby-client?logo=npm)](https://www.npmjs.com/package/obby-client)
[![PyPI](https://img.shields.io/pypi/v/obby-client?logo=pypi&logoColor=white)](https://pypi.org/project/obby-client/)
[![pub.dev](https://img.shields.io/pub/v/obby_client?logo=dart)](https://pub.dev/packages/obby_client)
[![License](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)

**Write the interface. This handles IRC.**

An IRCv3 engine with the client model built in, for Rust, C, TypeScript, Python and Dart. It parses
the protocol, negotiates capabilities, authenticates, and keeps the state you would otherwise write
yourself: channels, members, conversations, messages, dedup, history merging, and a reconnect that
replays what you had. Against an Obby server it also does voice signalling, end-to-end encryption
and link previews. Any IRC server works.

It does no I/O. You open the socket and you read the clock; you feed it bytes and the time, and it
tells you what happened and what to send. Same API in every language, so a second client is the UI
and a socket.

## What you get

- **One core, five languages.** The protocol is written once, in Rust. The five bindings cannot
  drift: a test fails when one of them is missing a command the others have.
- **Types, not strings.** Every command and event is typed in every language. The TypeScript
  definitions are generated from the Rust and contain no `any`, and a consumer is type-checked and
  then run against the built module on every push.
- **It remembers.** The engine holds the model, so an app renders it and keeps no second copy:
  scrollback with a retention cap, deduplicated replays, merged history pages, unread counts.

## Installation

<details>
<summary><b>Rust</b></summary>

```sh
cargo add obby-client
```

`obby-proto` is the wire format on its own: lines, tags, casemapping, ISUPPORT and modes, with no
connection and no state. Take it alone to parse IRC without running a client.

```sh
cargo add obby-proto
```

</details>

<details>
<summary><b>TypeScript, in the browser or in Bun</b></summary>

One WebAssembly build serves both.

```sh
npm install obby-client
```

Every published version reaches jsDelivr and unpkg the moment it reaches npm, with no account and
no setup, so a page loads it without a build step. An unversioned URL always serves the newest
release, and `init()` fetches the `.wasm` next to the module, which both CDNs serve:

```html
<script type="module">
  import init, { ObbyClient } from "https://cdn.jsdelivr.net/npm/obby-client/obby_wasm.js";
  await init();
</script>
```

Pin a version for anything you ship: write `obby-client@0.1.3` in place of `obby-client`. An
unpinned URL updates itself, so a later release would reach a page you have already shipped.

</details>

<details>
<summary><b>Python</b></summary>

Wheels are published for Linux, macOS and Windows, on CPython 3.9 and newer.

```sh
pip install obby-client
```

</details>

<details>
<summary><b>Dart and Flutter</b></summary>

```sh
dart pub add obby_client
```

The package calls the engine through its C ABI, so your application ships `libobby_ffi` and points
the client at it: `ObbyClient(config, libraryPath: "...")`. Every GitHub release includes a build
for Linux, macOS and Windows.

</details>

<details>
<summary><b>C</b></summary>

Every release includes an archive per platform with the header and the static and shared libraries:
<https://github.com/obbyworld/obby-client/releases>. To build them yourself:

```sh
cargo build -p obby-ffi --release
make header
```

The header is written to `bindings/obby-ffi/include/obby_ffi.h` and the libraries to
`target/release/libobby_ffi.{a,so,dylib}`.

</details>

## Usage

You feed the engine and drain it. In: `handle_bytes` with whatever the socket read, `tick` with the
time, and a method per command (`join`, `send_message`, and so on). Out: `poll_transmit` for bytes
to send, `poll_event` for what happened, `poll_timeout` for when to call `tick` again, so you can
sleep until then.

```rust
use std::io::{Read, Write};
use std::net::TcpStream;

use obby_client::{Client, Config, Event, Now};

// the engine never dials, so the server, the port and the transport are yours. Wrap this in TLS
// for 6697, which is what almost every network wants
let mut socket = TcpStream::connect(("irc.libera.chat", 6667))?;

let mut client = Client::new(Config::new("mynick"));
client.handle_connected();

let mut buf = [0u8; 8192];
loop {
    while let Some(bytes) = client.poll_transmit() {
        socket.write_all(&bytes)?;
    }

    let read = socket.read(&mut buf)?;
    client.handle_bytes(&buf[..read]);
    client.tick(Now { monotonic_ms, unix_ms });

    while let Some(event) = client.poll_event() {
        if let Event::Registered { .. } = event {
            client.join("#obby", None);
            client.send_message("#obby", "hello");
        }
    }
}
```

`Config` is where the nick, the fallback nicks, the password, the SASL credentials and the message
retention live. Nothing in it names a server, because the engine never opens one.

A full connection against a real server, TLS included, is
[`crates/obby-client/tests/live.rs`](crates/obby-client/tests/live.rs).

The model is readable at any moment through `client.model()`: every channel, who is in it, every
conversation, and the messages, capped per target by the retention you configure.

<details>
<summary><b>The same loop in TypeScript</b></summary>

```ts
import init, { ObbyClient, type ObbyEvent } from "obby-client";

await init();

// a browser reaches IRC over a WebSocket, so the server is whatever your network puts there
const socket = new WebSocket("wss://irc.example.org/webirc");
socket.binaryType = "arraybuffer";

const client = new ObbyClient({ nick: "mynick" });
socket.onopen = () => client.handleConnected();

socket.onmessage = (message) => {
  client.handleBytes(new Uint8Array(message.data));
  for (const event of client.pollEvents()) render(event);
  for (let bytes; (bytes = client.pollTransmit()); ) socket.send(bytes);
};

client.join("#obby");
```

Events drain as a batch, because one call across the WebAssembly boundary costs the same for one
event as for a hundred.

The package is strictly typed, and nothing in it is `any`. `Command`, `Event`, `Model`, `Config` and
every shape they reach are generated from the Rust types, so TypeScript rejects a misspelled field
before the code runs, and a change to the engine shows up as a type error:

```ts
for (const event of client.pollEvents()) {
  if (event.type === "registered") {
    // TypeScript knows this branch has `nick`, and that a join needs `channel` and `key`
    console.log(`registered as ${event.nick}`);
    client.join("#obby");
  }
}
```

The engine holds WebAssembly memory that the JavaScript collector knows nothing about, so release it
when you are done: `client.free()`, or `using client = new ObbyClient(...)` where your runtime
supports it. New event and command shapes arrive in minor releases, so keep a `default` branch
in a switch over them.

</details>

<details>
<summary><b>The same loop in Python</b></summary>

```python
import socket as socketlib

from obby_client import Client

sock = socketlib.create_connection(("irc.example.org", 6667))

client = Client({"nick": "mynick"})
client.handle_connected()

while (out := client.poll_transmit()) is not None:
    sock.sendall(out)

client.handle_bytes(sock.recv(4096))
for event in client.poll_events():
    render(event)

client.join("#obby")
client.tick(monotonic_ms, unix_ms)
```

</details>

<details>
<summary><b>The same loop in Dart</b></summary>

```dart
import 'package:obby_client/obby_client.dart';

final socket = await Socket.connect('irc.example.org', 6667);

final client = ObbyClient({'nick': 'mynick'});
client.handleConnected();

socket.listen((data) {
  client.handleBytes(data);
  for (final event in client.pollEvents()) render(event);
  for (var bytes = client.pollTransmit(); bytes != null; bytes = client.pollTransmit()) {
    socket.add(bytes);
  }
});

client.close();
```

The native handle is not synchronised, so one client belongs to one isolate. Give each isolate its
own.

</details>

<details>
<summary><b>The same loop in C</b></summary>

```c
#include "obby_ffi.h"

// the socket is yours: any host, any port, TLS or not
int fd = connect_to("irc.example.org", 6667);

obby_config_t config = {.nick = "mynick"};
obby_client_t *client = obby_client_new(&config);
obby_client_handle_connected(client);

obby_bytes_t out = obby_client_poll_transmit(client);
write(fd, out.ptr, out.len);
obby_client_free_bytes(out);

obby_client_handle_bytes(client, buffer, length);

obby_event_t *event = obby_client_poll_event(client);
if (obby_event_get_kind(event) == OBBY_EVENT_KIND_REGISTERED) {
    printf("registered as %s\n", obby_event_text(event, OBBY_EVENT_FIELD_NICK));
    obby_client_join(client, "#obby", NULL);
}
obby_event_free(event);

obby_client_send_message(client, "#obby", "hello");
obby_client_free(client);
```

Config, commands and events are C types. `obby_event_get_kind` says what an event is,
`obby_event_text` and `obby_event_number` read its fields, and `obby_event_json` gives the whole
body for the parts the ABI does not flatten, such as a voice frame. JSON stays available for the
long tail through `obby_client_command_from_json` and `obby_client_new_from_json`.

A handle is not synchronised. Never touch one from two threads at once, not even for two calls that
only read. Give each thread its own handle, or take your own lock.

[`bindings/obby-ffi/tests/smoke.c`](bindings/obby-ffi/tests/smoke.c) is a complete program, built
and run by `make c-smoke`.

</details>

## Documentation

<https://obbyworld.github.io/obby-client> has the reference for every language, rebuilt from the
code on each push: Rust, TypeScript, Python, Dart and the C header. The same site serves
[`llms.txt`](https://obbyworld.github.io/obby-client/llms.txt): every type that crosses a binding
and the whole C ABI in one file, for an agent that would rather read one page than crawl five.

## Examples

[`crates/obby-client/examples/echo-bot.rs`](crates/obby-client/examples/echo-bot.rs) is a working
client in one file: it connects over TCP, joins a channel, prints what people say and answers
anyone who says hello.

```sh
cargo run --example echo-bot -- irc.libera.chat:6667 '#obby' mynick
```

The same loop in TypeScript is
[`bindings/obby-wasm/tests/typecheck.ts`](bindings/obby-wasm/tests/typecheck.ts), and in C it is
[`bindings/obby-ffi/tests/smoke.c`](bindings/obby-ffi/tests/smoke.c). Both are compiled and run by
CI, so neither can drift from the API.

## Development

```sh
make check   # after every change
make test
make ci      # everything CI runs, before a commit
make live    # smoke test against a real server, needs the network
make site    # build the documentation site into target/site
```

`make help` lists the rest. [CONTRIBUTING.md](CONTRIBUTING.md) covers releasing and publishing.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
