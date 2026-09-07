# obby-client

**Write the interface. This handles IRC.**

An IRCv3 engine with the client model built in. It parses the protocol, negotiates capabilities,
authenticates, and keeps channels, members, conversations and their messages. It does no I/O: you
feed it bytes and the time, it tells you what happened and what to send. The engine is Rust, so the
protocol work does not run in Python.

```sh
pip install obby-client
```

```python
from obby_client import Client

client = Client({"nick": "mynick"})
client.handle_connected()

while (out := client.poll_transmit()) is not None:
    sock.sendall(out)

client.handle_bytes(sock.recv(4096))
for event in client.poll_events():
    print(event)

client.join("#obby")
client.tick(monotonic_ms, unix_ms)
```

## Build from source

```sh
maturin build --release --manifest-path bindings/obby-python/Cargo.toml
```
