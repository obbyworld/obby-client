// The driver against a fake socket: no real network, just the `send`/`onmessage`/`onclose` shape.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init from "../pkg/obby_wasm.js";
import { connect } from "../pkg/driver.js";

const wasm = fileURLToPath(new URL("../pkg/obby_wasm_bg.wasm", import.meta.url));
await init({ module_or_path: readFileSync(wasm) });

class FakeSocket {
  sent = [];
  onmessage = null;
  onclose = null;
  onopen = null;
  send(data) {
    this.sent.push(data);
  }
  close() {
    this.onclose?.();
  }
}

const socket = new FakeSocket();
const { client, events, close } = connect(socket, { nick: "driver" });

socket.onopen();
assert.ok(socket.sent.length > 0, "connecting queues the registration burst");
const first = new TextDecoder().decode(socket.sent[0]);
assert.ok(first.startsWith("CAP LS 302"), "the registration burst reaches the fake socket");

const stream = events();
socket.onmessage({ data: new TextEncoder().encode(":server 001 driver :Welcome\r\n") });
const { value: registered } = await stream.next();
assert.equal(registered.type, "registered", "a 001 produces a registered event on the stream");
assert.equal(registered.nick, "driver");
assert.equal(client.model().me.nick, "driver", "the underlying client is reachable directly");

close();

// a socket handed over already open never fires onopen again
class OpenSocket extends FakeSocket {
  readyState = 1;
}

const open = new OpenSocket();
const started = connect(open, { nick: "already" });
assert.ok(
  open.sent.length > 0,
  "a socket that is already open still gets the registration burst",
);
started.close();

// closing detaches the handlers, so a late message cannot reach a freed client
const late = new FakeSocket();
const ending = connect(late, { nick: "late" });
late.onopen();
ending.close();
assert.equal(late.onmessage, null, "close detaches the message handler before freeing the client");
late.onclose?.();

console.log("the driver drives the registration burst and the event stream");
