# obby-client

**Write the interface. This handles IRC.**

An IRCv3 engine with the client model built in, compiled to WebAssembly. It parses the protocol,
negotiates capabilities, authenticates, and keeps channels, members, conversations and their
messages. It does no I/O: you feed it bytes and the time, it tells you what happened and what to
send. One build serves both the browser and Bun, and every shape it hands you is typed.

```sh
npm install obby-client
```

The type definitions ship with the package and are generated from the Rust, so a change to the
engine reaches you as a type error.

```ts
import init, { ObbyClient, type Command, type ObbyEvent } from "obby-client";

await init();
const client = new ObbyClient({ nick: "mynick" });
client.handleConnected();

socket.onmessage = (message) => {
  client.handleBytes(new Uint8Array(message.data));
  for (const event of client.pollEvents()) render(event);
  for (let bytes; (bytes = client.pollTransmit()); ) socket.send(bytes);
};

client.command({ type: "join", channel: "#obby", key: null });
```

`client.model()` returns everything the connection knows: channels, members, conversations and
their messages. `client.pollTimeout()` says when `client.tick(performance.now(), Date.now())` next matters,
so a host can sleep until then.

## Build from source

```sh
wasm-pack build --target web
```
