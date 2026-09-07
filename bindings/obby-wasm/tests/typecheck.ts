// A consumer that has to keep compiling.
//
// The package's types are generated from the Rust, so this file is what proves they are still
// usable: every shape a host touches, under `tsc --strict`, with no `any` anywhere.

import init, { ObbyClient, type Command, type Model, type ObbyEvent } from "../pkg/obby_wasm.js";

await init();

// only the nick is required, and every other field has a default
const client = new ObbyClient({ nick: "typed", retention: 200 });

client.handleConnected();

// every command is a method, with the arguments it actually takes
client.join("#obby");
client.sendMessage("#obby", "hello");
client.setTyping("#obby", "active");
client.fetchHistory("#obby", null, 50);
client.markRead("#obby", Date.now());
client.sendVoiceSignal("^general", { type: "join", channel: "^general" });
client.watchNicks(["alice", "bob"]);

// and the generic form stays, for a command a host builds itself
const join: Command = { type: "join", channel: "#obby", key: null };
client.command(join);

const events: ObbyEvent[] = client.pollEvents();
for (const event of events) {
  switch (event.type) {
    case "registered":
      console.log(event.nick);
      break;
    case "model_changed":
      if (event.change.type === "message_added") {
        console.log(event.change.target, event.change.key.seq);
      }
      break;
    case "server_reply":
      console.log(event.severity, event.code, event.text);
      break;
    default:
      break;
  }
}

const bytes: Uint8Array | undefined = client.pollTransmit();
client.handleBytes(bytes ?? new Uint8Array());
client.tick(performance.now(), Date.now());

const timeout: number | undefined = client.pollTimeout();
const model: Model = client.model();
console.log(timeout, model.me.nick, Object.keys(model.channels).length);

client.free();
