#!/usr/bin/env bash
# Build the documentation site: one page per language, plus a single file an agent can read whole.
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

out="target/site"
rm -rf "$out"
mkdir -p "$out"

version=$(awk '
  /^\[/ { in_pkg = ($0 == "[workspace.package]") }
  in_pkg && /^version = "/ { gsub(/^version = "|"$/, ""); print; exit }
' Cargo.toml)

echo "building the rust reference"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cp -r target/doc "$out/rust"

echo "building the typescript reference"
make wasm >/dev/null
mkdir -p "$out/typescript"
npx --yes -p typedoc@0.28 -p typescript@5 typedoc \
  --out "$out/typescript" \
  --name "obby-client for TypeScript" \
  --readme bindings/obby-wasm/README.md \
  --skipErrorChecking \
  bindings/obby-wasm/pkg/obby_wasm.d.ts

if command -v maturin >/dev/null && command -v pip >/dev/null; then
  echo "building the python reference"
  # a fresh directory each time, so an older wheel left over from a previous build cannot be
  # picked up alongside this one
  rm -rf target/site-wheels
  maturin build --release --manifest-path bindings/obby-python/Cargo.toml --out target/site-wheels >/dev/null
  pip install --quiet --force-reinstall target/site-wheels/*.whl
  pip install --quiet pdoc
  pdoc --output-directory "$out/python" obby_client
else
  echo "skipping the python reference: maturin or pip is missing"
fi

echo "building the dart reference"
cargo build -p obby-ffi --release >/dev/null
(cd bindings/obby-dart && dart pub get >/dev/null && dart doc --output "$repo_root/$out/dart")

echo "collecting the c header"
make header >/dev/null
mkdir -p "$out/c"
cp bindings/obby-ffi/include/obby_ffi.h "$out/c/"

# one file with the whole surface in it, because an agent reading this repo should not have to
# crawl four generated sites to answer a question about the API
echo "writing llms.txt"
{
  echo "# obby-client ${version}"
  echo
  sed -n '/^A full IRCv3 client engine/,/^## Installation/p' README.md | sed '$d'
  echo "## Where the reference lives"
  echo
  echo "- Rust: https://obbyworld.github.io/obby-client/rust/obby_client/"
  echo "- TypeScript: https://obbyworld.github.io/obby-client/typescript/"
  echo "- Python: https://obbyworld.github.io/obby-client/python/obby_client.html"
  echo "- Dart: https://obbyworld.github.io/obby-client/dart/"
  echo "- C: https://obbyworld.github.io/obby-client/c/obby_ffi.h"
  echo
  echo "## Every type that crosses a binding"
  echo
  echo "Generated from the Rust. The same shapes reach TypeScript, Python, Dart and C as JSON."
  echo
  echo '```typescript'
  cat bindings/obby-wasm/src/types.d.ts
  echo '```'
  echo
  echo "## The C ABI"
  echo
  echo '```c'
  cat bindings/obby-ffi/include/obby_ffi.h
  echo '```'
} >"$out/llms.txt"

echo "writing the index"
cat >"$out/index.html" <<HTML
<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>obby-client, the IRCv3 and Obby protocol engine</title>
<meta name="description" content="An IRCv3 client engine to build a chat client on, from Rust, C, TypeScript, Python or Dart.">
<style>
  :root {
    color-scheme: dark light;
    --bg: #0f1115;
    --panel: #161a21;
    --line: #262c36;
    --ink: #e6e9ef;
    --dim: #98a2b3;
    --accent: #7aa2f7;
    --accent-soft: #1d2433;
    --mono: ui-monospace, SFMono-Regular, "SF Mono", Menlo, monospace;
  }
  @media (prefers-color-scheme: light) {
    :root {
      --bg: #ffffff; --panel: #f6f7f9; --line: #e3e6ea;
      --ink: #14181f; --dim: #5b6472; --accent: #2b5fd9; --accent-soft: #eef2fd;
    }
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; background: var(--bg); color: var(--ink);
    font: 16px/1.65 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
    -webkit-font-smoothing: antialiased;
  }
  .wrap { max-width: 60rem; margin: 0 auto; padding: 0 1.5rem; }
  header { padding: 5rem 0 3rem; border-bottom: 1px solid var(--line); }
  h1 { margin: 0; font-size: clamp(2.2rem, 6vw, 3.4rem); letter-spacing: -0.03em; }
  h1 .dot { color: var(--accent); }
  .tag { margin: 0 0 1.5rem; font-family: var(--mono); font-size: .8rem;
         letter-spacing: .12em; text-transform: uppercase; color: var(--dim); }
  .lede { margin: 1.25rem 0 0; max-width: 42rem; font-size: 1.15rem; color: var(--ink); }
  .lede + .lede { color: var(--dim); font-size: 1rem; }
  .badges { display: flex; flex-wrap: wrap; gap: .4rem; margin-top: 2rem; }
  .badges img { height: 20px; }
  section { padding: 3.5rem 0; border-bottom: 1px solid var(--line); }
  h2 { margin: 0 0 1.5rem; font-size: .8rem; font-family: var(--mono);
       letter-spacing: .12em; text-transform: uppercase; color: var(--dim); font-weight: 600; }
  .grid { display: grid; gap: 1rem; grid-template-columns: repeat(auto-fit, minmax(15rem, 1fr)); }
  a.card {
    display: block; padding: 1.1rem 1.25rem; text-decoration: none; color: inherit;
    background: var(--panel); border: 1px solid var(--line); border-radius: 10px;
    transition: border-color .15s ease, transform .15s ease;
  }
  a.card:hover { border-color: var(--accent); transform: translateY(-2px); }
  div.card {
    padding: 1.1rem 1.25rem; background: var(--panel);
    border: 1px solid var(--line); border-radius: 10px;
  }
  div.card .name { font-weight: 600; font-size: 1.05rem; }
  div.card .what { display: block; margin-top: .3rem; color: var(--dim); font-size: .9rem; }
  a.card .name { font-weight: 600; font-size: 1.05rem; }
  a.card .what { display: block; margin-top: .3rem; color: var(--dim); font-size: .9rem; }
  a.card code { font-family: var(--mono); font-size: .82rem; color: var(--accent); }
  pre {
    margin: 0; padding: 1.15rem 1.25rem; overflow-x: auto;
    background: var(--panel); border: 1px solid var(--line); border-radius: 10px;
    font-family: var(--mono); font-size: .88rem; line-height: 1.6;
  }
  pre .c { color: var(--dim); }
  pre .k { color: var(--accent); }
  .two { display: grid; gap: 1rem; grid-template-columns: repeat(auto-fit, minmax(20rem, 1fr)); }
  footer { padding: 2.5rem 0 4rem; color: var(--dim); font-size: .9rem; }
  footer a { color: var(--accent); }
</style>
<div class="wrap">
  <header>
    <p class="tag">version ${version} &middot; GPL-3.0-or-later</p>
    <h1>obby<span class="dot">-</span>client</h1>
    <p class="lede">Write the interface. This handles IRC.</p>
    <p class="lede">An IRCv3 engine with the client model built in, for Rust, C, TypeScript, Python
      and Dart. It does no I/O: you feed it bytes and the time, it tells you what happened and what
      to send. Works against any IRC server.</p>
    <div class="badges">
      <a href="https://crates.io/crates/obby-client"><img alt="crates.io" src="https://img.shields.io/crates/v/obby-client?logo=rust"></a>
      <a href="https://www.npmjs.com/package/obby-client"><img alt="npm" src="https://img.shields.io/npm/v/obby-client?logo=npm"></a>
      <a href="https://pypi.org/project/obby-client/"><img alt="PyPI" src="https://img.shields.io/pypi/v/obby-client?logo=pypi&logoColor=white"></a>
      <a href="https://pub.dev/packages/obby_client"><img alt="pub.dev" src="https://img.shields.io/pub/v/obby_client?logo=dart"></a>
      <a href="https://github.com/obbyworld/obby-client/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/obbyworld/obby-client/actions/workflows/ci.yml/badge.svg"></a>
    </div>
  </header>

  <section>
    <h2>Why</h2>
    <div class="grid">
      <div class="card">
        <span class="name">One core, five languages</span>
        <span class="what">The protocol is written once, in Rust. A new client is the UI plus a
          socket, and the five bindings cannot drift: a test fails when one of them lags.</span>
      </div>
      <div class="card">
        <span class="name">Types, not strings</span>
        <span class="what">Commands and events are typed in every language. The TypeScript
          definitions are generated from the Rust and contain no <code>any</code>.</span>
      </div>
      <div class="card">
        <span class="name">It remembers</span>
        <span class="what">Channels, members, conversations and their messages, with dedup, history
          merging and a reconnect that replays what you had.</span>
      </div>
    </div>
  </section>

  <section>
    <h2>Reference</h2>
    <div class="grid">
      <a class="card" href="rust/obby_client/">
        <span class="name">Rust</span>
        <span class="what">The engine and the model. <code>cargo add obby-client</code></span>
      </a>
      <a class="card" href="typescript/">
        <span class="name">TypeScript</span>
        <span class="what">Browser and Bun, fully typed. <code>npm i obby-client</code></span>
      </a>
      <a class="card" href="python/obby_client.html">
        <span class="name">Python</span>
        <span class="what">Wheels for every platform. <code>pip install obby-client</code></span>
      </a>
      <a class="card" href="dart/">
        <span class="name">Dart</span>
        <span class="what">Over the C ABI. <code>dart pub add obby_client</code></span>
      </a>
      <a class="card" href="c/obby_ffi.h">
        <span class="name">C</span>
        <span class="what">The generated header, typed config, commands and events</span>
      </a>
      <a class="card" href="rust/obby_proto/">
        <span class="name">obby-proto</span>
        <span class="what">The wire format alone: lines, tags, casemapping, ISUPPORT</span>
      </a>
    </div>
  </section>

  <section>
    <h2>The whole interface</h2>
    <div class="two">
<pre><span class="c">// bytes and time go in</span>
client.<span class="k">handleConnected</span>();
client.<span class="k">handleBytes</span>(chunk);
client.<span class="k">tick</span>(monotonicMs, unixMs);

<span class="c">// bytes, events and the next wake-up come out</span>
client.<span class="k">pollTransmit</span>();
client.<span class="k">pollEvents</span>();
client.<span class="k">pollTimeout</span>();</pre>
<pre><span class="c">// every command is a method</span>
client.<span class="k">join</span>(<span class="c">"#obby"</span>);
client.<span class="k">sendMessage</span>(<span class="c">"#obby"</span>, <span class="c">"hello"</span>);
client.<span class="k">setTyping</span>(<span class="c">"#obby"</span>, <span class="c">"active"</span>);
client.<span class="k">fetchHistory</span>(<span class="c">"#obby"</span>, <span class="k">null</span>, 50);

<span class="c">// and the model is always readable</span>
client.<span class="k">model</span>().channels;</pre>
    </div>
  </section>

  <section>
    <h2>In a page, with no build step</h2>
<pre>&lt;script type="module"&gt;
  <span class="k">import</span> init, { ObbyClient } <span class="k">from</span>
    <span class="c">"https://cdn.jsdelivr.net/npm/obby-client/obby_wasm.js"</span>;
  <span class="k">await</span> init();
&lt;/script&gt;</pre>
  </section>

  <section>
    <h2>Examples</h2>
    <div class="grid">
      <a class="card" href="https://github.com/obbyworld/obby-client/blob/main/crates/obby-client/examples/echo-bot.rs">
        <span class="name">echo-bot.rs</span>
        <span class="what">A working client in one file: connect, join, answer anyone who says
          hello. <code>cargo run --example echo-bot</code></span>
      </a>
      <a class="card" href="https://github.com/obbyworld/obby-client/blob/main/bindings/obby-ffi/tests/smoke.c">
        <span class="name">smoke.c</span>
        <span class="what">The same loop in C, compiled and run by CI on every push</span>
      </a>
      <a class="card" href="https://github.com/obbyworld/obby-client/blob/main/bindings/obby-wasm/tests/typecheck.ts">
        <span class="name">typecheck.ts</span>
        <span class="what">The same loop in TypeScript, type-checked and run against the built
          module</span>
      </a>
    </div>
  </section>

  <section>
    <h2>For agents</h2>
    <div class="grid">
      <a class="card" href="llms.txt">
        <span class="name">llms.txt</span>
        <span class="what">Every type that crosses a binding and the whole C ABI, in one file</span>
      </a>
      <a class="card" href="https://github.com/obbyworld/obby-client">
        <span class="name">Source</span>
        <span class="what">The engine, the bindings, the transcripts it is tested against</span>
      </a>
    </div>
  </section>

  <footer>
    Built from the code on every push. Protocol: <a href="https://ircv3.net/irc/">IRCv3</a>,
    <a href="https://modern.ircdocs.horse/">Modern IRC</a>, and the
    <a href="https://github.com/obbyworld/extensions">Obby extensions</a>.
  </footer>
</div>
HTML

echo "the site is in $out"
