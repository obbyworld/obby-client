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
  maturin build --release --manifest-path bindings/obby-python/Cargo.toml --out target/wheels >/dev/null
  pip install --quiet --force-reinstall target/wheels/*.whl
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
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>obby-client ${version}</title>
<style>
  :root { color-scheme: light dark; }
  body { margin: 0 auto; padding: 3rem 1.25rem; max-width: 44rem;
         font: 16px/1.6 ui-sans-serif, system-ui, sans-serif; }
  h1 { margin-bottom: 0; font-size: 1.6rem; }
  p.version { margin-top: .25rem; opacity: .7; }
  ul { list-style: none; padding: 0; }
  li { margin: .75rem 0; }
  a { text-decoration: none; }
  a:hover { text-decoration: underline; }
  code { font-family: ui-monospace, monospace; font-size: .9em; }
  .what { opacity: .75; }
</style>
<h1>obby-client</h1>
<p class="version">${version}</p>
<p>The IRCv3 and Obby protocol engine, as one Rust core with bindings for C, TypeScript, Python and
Dart. It opens no socket, reads no clock and draws nothing.</p>
<ul>
  <li><a href="rust/obby_client/">Rust</a> <span class="what">the engine, and
    <a href="rust/obby_proto/">obby-proto</a> for the wire format alone</span></li>
  <li><a href="typescript/">TypeScript</a> <span class="what">the browser and Bun package</span></li>
  <li><a href="python/obby_client.html">Python</a> <span class="what">the CPython wheel</span></li>
  <li><a href="dart/">Dart</a> <span class="what">over the C ABI</span></li>
  <li><a href="c/obby_ffi.h">C</a> <span class="what">the generated header</span></li>
  <li><a href="llms.txt">llms.txt</a> <span class="what">every type and the whole C ABI in one
    file, for an agent</span></li>
  <li><a href="https://github.com/obbyworld/obby-client">Source</a></li>
</ul>
HTML

echo "the site is in $out"
