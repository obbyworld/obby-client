# obby-client

The Obby IRCv3 client engine. It parses the protocol, drives the connection and holds the client
model. It opens no socket and keeps no clock, so the host feeds it bytes and time.

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

client.command({"type": "join", "channel": "#obby", "key": None})
client.tick(monotonic_ms, unix_ms)
```

## Build from source

```sh
maturin build --release --manifest-path bindings/obby-python/Cargo.toml
```
