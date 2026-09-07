.DEFAULT_GOAL := help

# a Rust staticlib carries no dependencies of its own, so the platform's libraries come last
FFI_SYSTEM_LIBS := $(if $(filter Darwin,$(shell uname -s)),-framework CoreFoundation -framework Security,-lpthread -ldl -lm)
.PHONY: help install fix precommit check test live snap snap-accept doc lint fmt-check features wasm-check header msrv deny dupes machete \
        wasm python dart c-smoke ts-types ts-check ci release-patch release-minor release-major

help: ## list available targets
	@grep -E '^[a-zA-Z0-9_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-14s %s\n", $$1, $$2}'

install: ## install the dev toolchain and git hooks
	cargo install cargo-nextest cargo-deny cargo-machete cargo-insta --locked
	uv tool install pre-commit
	pre-commit install

# --- inner loop ---

check: ## the one command to run after every change
	cargo clippy --workspace --all-targets --all-features -- -D warnings

test: ## unit, integration and doc tests
	# ts-rs generates one test per exported type, and they all write the same file, which only
	# holds together in one process. `make ts-types` runs them under cargo test for that reason
	cargo nextest run --workspace --all-features -E 'not test(/export_bindings_/)'
	cargo test --doc --workspace

live: ## smoke test against a real server, needs the network
	cargo test -p obby-client --test live --all-features -- --ignored --nocapture

snap: ## replay the golden transcripts and show what changed
	cargo insta test --workspace

snap-accept: ## accept the changed snapshots, after reading the diff
	cargo insta accept

fix: ## autofix what is mechanical
	cargo clippy --workspace --all-targets --all-features --fix --allow-dirty --allow-staged
	cargo fmt --all

precommit: fmt-check check ## hook entry

# --- checks, each mirrors one CI job ---

fmt-check:
	cargo fmt --all -- --check

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps

wasm-check: ## the core and its wasm binding must reach the browser; pyo3 never can
	cargo check -p obby-proto -p obby-client -p obby-wasm --all-features --target wasm32-unknown-unknown

features: ## every feature combination has to build, which --all-features hides
	cargo check -p obby-client --no-default-features
	for f in std std,serde std,obby std,obby,voice std,obby,e2ee std,serde,obby,voice,e2ee; do \
		cargo check -p obby-client --no-default-features --features "$$f" || exit 1; \
	done
	cargo check -p obby-proto --no-default-features
	cargo check -p obby-proto --no-default-features --features serde

msrv: ## the crate must build on the rust-version in Cargo.toml
	cargo +1.90 check --workspace --all-features

deny: ## licence and advisory hygiene
	cargo deny check

dupes: ## find copy-pasted code
	npx --yes jscpd@4 crates/

machete: ## unused dependencies
	cargo machete

ci: fmt-check check test doc features wasm-check deny dupes machete ## everything CI runs

# --- bindings ---

header: ## regenerate the C header from the ffi crate
	cbindgen --config bindings/obby-ffi/cbindgen.toml --crate obby-ffi --output bindings/obby-ffi/include/obby_ffi.h

ts-types: ## regenerate the TypeScript definitions from the Rust types
	rm -rf crates/obby-client/bindings
	cargo test -p obby-client --features ts export_bindings
	cp crates/obby-client/bindings/obby.ts bindings/obby-wasm/src/types.d.ts
	rm -rf crates/obby-client/bindings bindings/obby.ts

wasm: ## browser and bun package
	wasm-pack build bindings/obby-wasm --target web --out-dir pkg
	# wasm-pack names the package after the crate, and npm shows the name a consumer types
	cd bindings/obby-wasm/pkg && npm pkg set name=obby-client

c-smoke: ## compile and run the C program that drives the whole ABI
	cargo build -p obby-ffi --release
	cc -std=c11 -Wall -Wextra -Werror -D_GNU_SOURCE -Ibindings/obby-ffi/include \
	  bindings/obby-ffi/tests/smoke.c target/release/libobby_ffi.a \
	  $(FFI_SYSTEM_LIBS) -o target/obby-c-smoke
	./target/obby-c-smoke

ts-check: ## typecheck a consumer against the generated definitions
	npx --yes -p typescript@5 tsc --strict --noEmit --target es2022 --lib esnext,dom \
	  --module es2022 --moduleResolution bundler bindings/obby-wasm/tests/typecheck.ts

python: ## cpython wheel
	maturin build --release --manifest-path bindings/obby-python/Cargo.toml

dart: ## dart package, over the freshly built C ABI
	cargo build -p obby-ffi --release
	cd bindings/obby-dart && dart pub get && dart analyze && dart test

release-patch: ## bump the patch version, tag it, and push, which publishes everywhere
	scripts/release.sh patch

release-minor: ## bump the minor version, tag it, and push, which publishes everywhere
	scripts/release.sh minor

release-major: ## bump the major version, tag it, and push, which publishes everywhere
	scripts/release.sh major
