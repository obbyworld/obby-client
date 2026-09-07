// What the definitions promise, checked against what the module actually returns.
//
// `tsc` only checks the generated file against itself. This runs the module and asserts the shapes
// a consumer sees: plain objects rather than `Map`, `null` rather than `undefined`, and `number`
// rather than `BigInt`. Those three drifted apart once already.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, { ObbyClient } from "../pkg/obby_wasm.js";

const wasm = fileURLToPath(new URL("../pkg/obby_wasm_bg.wasm", import.meta.url));
await init({ module_or_path: readFileSync(wasm) });

const client = new ObbyClient({ nick: "runtime" });
client.connected();
client.handleBytes(
  new TextEncoder().encode(":server 001 runtime :Welcome\r\n:runtime JOIN #obby\r\n"),
);

const events = client.pollEvents();
assert.ok(Array.isArray(events), "pollEvents returns an array");
const registered = events.find((event) => event.type === "registered");
assert.equal(registered.nick, "runtime");

const model = client.model();
assert.equal(model.channels instanceof Map, false, "a map crosses as a plain object");
assert.ok("#obby" in model.channels, "the channel we joined is keyed by its folded name");
assert.equal(model.me.account, null, "an absent option crosses as null");
assert.equal(typeof model.next_seq, "number", "a 64-bit number crosses as a number");
assert.ok(JSON.stringify(model).length > 0, "the model survives JSON.stringify");

const channel = model.channels["#obby"];
assert.equal(channel.name, "#obby");
assert.equal(channel.members instanceof Map, false);
assert.ok(Array.isArray(channel.log.messages), "the log is a list");

client.command({ type: "message", target: "#obby", text: "hello" });
const sent = new TextDecoder().decode(client.pollTransmit());
assert.ok(sent.length > 0);

assert.equal(typeof client.pollTimeout(), "bigint", "the class methods keep wasm-bindgen's u64");

client.free();
console.log("the runtime shapes match the definitions");
