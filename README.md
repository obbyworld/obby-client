# obby-client

[![CI](https://github.com/obbyworld/obby-client/actions/workflows/ci.yml/badge.svg)](https://github.com/obbyworld/obby-client/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/obby-client.svg)](https://crates.io/crates/obby-client)
[![npm](https://img.shields.io/npm/v/obby-wasm.svg)](https://www.npmjs.com/package/obby-wasm)
[![PyPI](https://img.shields.io/pypi/v/obby-client.svg)](https://pypi.org/project/obby-client/)
[![pub.dev](https://img.shields.io/pub/v/obby_client.svg)](https://pub.dev/packages/obby_client)
[![License](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)

The IRCv3 and Obby protocol engine, as one Rust core with bindings for C, TypeScript, Python and
Dart. It parses the protocol, negotiates capabilities, authenticates, and holds everything a client
knows: channels, members, conversations, and their messages.

It opens no socket, reads no clock and draws nothing. Your app hands it bytes and the time, then
drains bytes to write, events to render, and the moment it next wants waking. That is the whole
interface, and it is the same interface in every language here, so a new Obby client is a user
interface and nothing else.

## Install

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
npm install obby-wasm
```

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
the client at it: `ObbyClient(config, libraryPath: "...")`. Every GitHub release carries a build for
Linux, macOS and Windows.

</details>

<details>
<summary><b>C</b></summary>

Every release carries an archive per platform with the header and the static and shared libraries:
<https://github.com/obbyworld/obby-client/releases>. To build them yourself:

```sh
cargo build -p obby-ffi --release
make header
```

The header lands in `bindings/obby-ffi/include/obby_ffi.h` and the libraries in
`target/release/libobby_ffi.{a,so,dylib}`.

</details>

## Use it

The engine is a state machine you feed and drain. Four ways in: construction, `command` for what the
user wants, `handle_bytes` for whatever the socket read, and `tick` for the time. Three ways out:
`poll_transmit` for bytes to write, `poll_event` for what happened, and `poll_timeout` for when
`tick` next matters, so a host sleeps exactly rather than spinning.

```rust
use obby_client::{Client, Command, Config, Event, Now};

let mut client = Client::new(Config::new("mynick"));
client.connected();

loop {
    while let Some(bytes) = client.poll_transmit() {
        socket.write_all(&bytes)?;
    }

    let read = socket.read(&mut buf)?;
    client.handle_bytes(&buf[..read]);
    client.tick(Now { monotonic_ms, unix_ms });

    while let Some(event) = client.poll_event() {
        if let Event::Registered { .. } = event {
            client.command(Command::Join { channel: "#obby".into(), key: None });
        }
    }
}
```

The model is readable at any moment through `client.model()`: every channel, who is in it, every
conversation, and the messages, capped per target by the retention you configure.

<details>
<summary><b>The same loop in TypeScript</b></summary>

```js
import init, { ObbyClient } from "obby-wasm";

await init();
const client = new ObbyClient({ nick: "mynick" });
client.connected();

socket.onmessage = (message) => {
  client.handleBytes(new Uint8Array(message.data));
  for (const event of client.pollEvents()) render(event);
  for (let bytes; (bytes = client.pollTransmit()); ) socket.send(bytes);
};

client.command({ command: "join", channel: "#obby", key: null });
```

Events drain as a batch, because one call across the WebAssembly boundary costs the same whether it
carries one event or a hundred.

</details>

<details>
<summary><b>The same loop in Python</b></summary>

```python
from obby_client import Client

client = Client({"nick": "mynick"})
client.connected()

while (out := client.poll_transmit()) is not None:
    sock.sendall(out)

client.handle_bytes(sock.recv(4096))
for event in client.poll_events():
    render(event)

client.command({"command": "join", "channel": "#obby", "key": None})
client.tick(monotonic_ms, unix_ms)
```

</details>

<details>
<summary><b>The same loop in Dart</b></summary>

```dart
import 'package:obby_client/obby_client.dart';

final client = ObbyClient({'nick': 'mynick'});
client.connected();

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

obby_client_t *client = obby_client_new("{\"nick\":\"mynick\"}");
obby_client_connected(client);

obby_bytes_t out = obby_client_poll_transmit(client);
write(fd, out.ptr, out.len);
obby_client_free_bytes(out);

obby_client_handle_bytes(client, buffer, length);
char *events = obby_client_poll_events(client);
obby_client_free_string(events);

obby_client_free(client);
```

A handle is not synchronised. Never touch one from two threads at once, not even for two calls that
only read. Give each thread its own handle, or take your own lock.

</details>

## Develop

```sh
make check   # after every change
make test
make ci      # the gate before a commit
make live    # smoke test against a real server, needs the network
```

`make help` lists the rest. [CONTRIBUTING.md](CONTRIBUTING.md) covers releasing and publishing.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
