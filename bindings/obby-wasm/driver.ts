// The manual drain loop, written once.
//
// `ObbyClient` is sans-io: something has to call `pollTransmit` after every input, tick the clock,
// and schedule the next `tick` from `pollTimeout()`. That loop is the same in every host, so this
// writes it once for every consumer. The manual loop in the package README keeps working; this is
// an optional convenience over it.

import { ObbyClient, type Config, type ObbyEvent } from "./pkg/obby_wasm.js";

/** The slice of `WebSocket` the driver needs, so a Node `ws` socket fits without a DOM lib. */
export interface SocketLike {
  send(data: Uint8Array): void;
  close(): void;
  /** `WebSocket.OPEN` is 1 everywhere; a transport that does not report one is treated as opening. */
  readonly readyState?: number;
  onmessage: ((event: { data: unknown }) => void) | null;
  onclose: (() => void) | null;
  onopen: (() => void) | null;
}

/** A live connection: commands go through `client`, events come out of `events()`. */
export interface DriverHandle {
  readonly client: ObbyClient;
  events(): AsyncGenerator<ObbyEvent, void, void>;
  close(): void;
}

/** What `readyState` reads when a socket is already usable. */
const OPEN = 1;

/** Bytes out of `event.data`, whatever shape the transport handed us. */
function toBytes(data: unknown): Uint8Array {
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (data instanceof Uint8Array) return data;
  if (typeof data === "string") return new TextEncoder().encode(data);
  throw new TypeError("socket message was neither binary nor text");
}

/**
 * Drive an `ObbyClient` off a socket that is already open (or about to open). Owns the poll loop:
 * flushes `pollTransmit` after every input, ticks the clock on every message, and schedules the
 * next `tick` from `pollTimeout()` with `setTimeout`, so the loop sleeps until the engine has
 * something to do.
 *
 * @example
 * ```ts
 * const socket = new WebSocket("wss://irc.example.org/webirc");
 * socket.binaryType = "arraybuffer";
 * const { client, events, close } = connect(socket, { nick: "mynick" });
 * for await (const event of events()) {
 *   if (event.type === "registered") client.join("#obby");
 * }
 * ```
 */
export function connect(socket: SocketLike, config: Config): DriverHandle {
  const client = new ObbyClient(config);
  const queue: ObbyEvent[] = [];
  let wake: (() => void) | null = null;
  let closed = false;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const flush = () => {
    for (let bytes = client.pollTransmit(); bytes; bytes = client.pollTransmit()) {
      socket.send(bytes);
    }
  };

  const scheduleTick = () => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    const deadline = client.pollTimeout();
    if (deadline === undefined || closed) return;
    const delay = Math.max(0, deadline - performance.now());
    timer = setTimeout(() => {
      client.tick(performance.now(), Date.now());
      flush();
      drain();
      scheduleTick();
    }, delay);
  };

  const drain = () => {
    for (const event of client.pollEvents()) queue.push(event);
    if (queue.length > 0 && wake) {
      const resolve = wake;
      wake = null;
      resolve();
    }
  };

  const start = () => {
    client.handleConnected();
    flush();
    scheduleTick();
  };

  socket.onopen = start;

  socket.onmessage = (event) => {
    if (closed) return;
    client.handleBytes(toBytes(event.data));
    client.tick(performance.now(), Date.now());
    flush();
    drain();
    scheduleTick();
  };

  socket.onclose = () => {
    if (closed) return;
    client.handleDisconnected();
    closed = true;
    if (timer !== null) clearTimeout(timer);
    if (wake) {
      const resolve = wake;
      wake = null;
      resolve();
    }
  };

  // a socket handed to us already open never fires `onopen` again, and the registration burst would
  // sit in the engine forever waiting for it
  if (socket.readyState === OPEN) start();

  async function* events(): AsyncGenerator<ObbyEvent, void, void> {
    while (!closed || queue.length > 0) {
      if (queue.length === 0) {
        await new Promise<void>((resolve) => {
          wake = resolve;
        });
        continue;
      }
      yield queue.shift() as ObbyEvent;
    }
  }

  return {
    client,
    events,
    close: () => {
      closed = true;
      if (timer !== null) clearTimeout(timer);
      // the socket delivers its close asynchronously, so a handler still attached would reach a
      // freed client
      socket.onmessage = null;
      socket.onclose = null;
      socket.onopen = null;
      socket.close();
      if (wake) {
        const resolve = wake;
        wake = null;
        resolve();
      }
      client.free();
    },
  };
}
