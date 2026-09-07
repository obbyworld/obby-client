# obby-wasm

The Obby IRCv3 client engine, as WebAssembly. It parses the protocol, drives the connection and
holds the client model. It opens no socket and keeps no clock, so you hand it bytes and the time.
One build serves both the browser and Bun.

```sh
npm install obby-wasm
```

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

`client.model()` returns everything the connection knows: channels, members, conversations and
their messages. `client.pollTimeout()` says when `client.tick(monotonicMs, unixMs)` next matters,
so a host sleeps exactly rather than spinning.

## Build from source

```sh
wasm-pack build --target web
```
