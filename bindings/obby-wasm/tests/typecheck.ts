// A consumer that has to keep compiling.
//
// The package's types are generated from the Rust, so this file is what proves they are still
// usable: every shape a host touches, under `tsc --strict`, with no `any` anywhere.

import init, { ObbyClient, type Command, type Model, type ObbyEvent } from "../pkg/obby_wasm.js";

await init();

// only the nick is required, and every other field has a default
const client = new ObbyClient({ nick: "typed", retention: 200 });

client.handleConnected();

const join: Command = { type: "join", channel: "#obby", key: null };
client.command(join);
client.command({ type: "message", target: "#obby", text: "hello" });

const events: ObbyEvent[] = client.pollEvents();
for (const event of events) {
  switch (event.type) {
    case "registered":
      console.log(event.nick);
      break;
    case "changed":
      if (event.change.type === "message") {
        console.log(event.change.target, event.change.key.seq);
      }
      break;
    case "reply":
      console.log(event.severity, event.code, event.text);
      break;
    default:
      break;
  }
}

const bytes: Uint8Array | undefined = client.pollTransmit();
client.handleBytes(bytes ?? new Uint8Array());
client.tick(0n, 0n);

const timeout: bigint | undefined = client.pollTimeout();
const model: Model = client.model();
console.log(timeout, model.me.nick, Object.keys(model.channels).length);

client.free();
