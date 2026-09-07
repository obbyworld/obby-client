# Contributing

Install the toolchain and the git hooks once:

```sh
make install
```

Run `make check` after every change and `make ci` before every commit. `make help` lists the rest.

The work is done when `make ci` passes, and a failing gate is the thing to fix, never the thing to
weaken. Nothing outside a test may `unwrap`, `expect` or `panic!`; clippy denies them. A changed
snapshot is a behaviour change, so read the diff with `make snap` and only then `make snap-accept`.

Comments explain why, never what. Commit messages are one line.

## First publish

Publishing holds no secret: every registry authenticates the release workflow with OIDC. Setting
that up is a one-time job per package, and it cannot be automated, because each registry needs a
human to say that this repository and this workflow file are allowed to publish.

Three of the four need the package to exist before the trust can be attached, so their first
release is published by hand with a personal credential. That credential is used once and never
stored here.

### crates.io

Get a token at <https://crates.io/settings/tokens>, `cargo login`, then publish in this order,
because `obby-client` and `obby-ffi` depend on `obby-proto` by version and cannot be packaged until
it resolves from the registry:

```sh
cargo publish -p obby-proto
cargo publish -p obby-client
cargo publish -p obby-ffi
```

Then on each of

- <https://crates.io/crates/obby-proto/settings>
- <https://crates.io/crates/obby-client/settings>
- <https://crates.io/crates/obby-ffi/settings>

add a Trusted Publisher: repository owner `obbyworld`, repository `obby-client`, workflow
`release.yml`, no environment.

### npm

Needs npm 11.5.1 or newer locally for the first publish.

```sh
npm login
make wasm
npm publish --access public bindings/obby-wasm/pkg
```

Then at <https://www.npmjs.com/package/obby-wasm/access>, under Trusted publisher, choose GitHub
Actions with organization `obbyworld`, repository `obby-client`, workflow `release.yml`.

### PyPI

PyPI is the only one that can create the project from the workflow's first run, so nothing is
published by hand. Fill in the pending publisher form at
<https://pypi.org/manage/account/publishing/>: project `obby-client`, owner `obbyworld`, repository
`obby-client`, workflow `release.yml`, environment blank.

### pub.dev

```sh
cd bindings/obby-dart && dart pub publish
```

Then at <https://pub.dev/packages/obby_client/admin>, enable automated publishing from GitHub
Actions with repository `obbyworld/obby-client` and tag pattern `v{{version}}`.

## Releasing

```sh
scripts/release.sh patch|minor|major
```

Bumps the version everywhere it is written, runs `make ci`, then commits, tags and pushes. The tag
push publishes the crates to crates.io, the wasm package to npm, wheels to PyPI, the Dart package to
pub.dev, and a C library archive per target to a GitHub release. Each registry publishes on its own,
so one being down does not hold up the rest.

Renaming `release.yml` breaks publishing until every registry's trusted-publisher entry is updated,
because the trust names the workflow file.
