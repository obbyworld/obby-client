#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $(basename "$0") patch|minor|major" >&2
  exit 1
}

[[ $# -eq 1 ]] || usage
bump="$1"
case "$bump" in
  patch | minor | major) ;;
  *) usage ;;
esac

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

cargo_toml="Cargo.toml"

if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: working tree is dirty, commit or stash first" >&2
  exit 1
fi

branch=$(git branch --show-current)
if [[ "$branch" != "main" ]]; then
  echo "error: release must run from main, currently on '${branch:-detached HEAD}'" >&2
  exit 1
fi

remote=$(git for-each-ref --format='%(upstream:remotename)' refs/heads/main)
remote=${remote:-origin}

git fetch --quiet "$remote" main

local_rev=$(git rev-parse HEAD)
remote_rev=$(git rev-parse "$remote/main")
if [[ "$local_rev" != "$remote_rev" ]] && git merge-base --is-ancestor "$local_rev" "$remote_rev"; then
  echo "error: main is behind $remote/main, pull first" >&2
  exit 1
fi

# the only version line in [workspace.package]; every crate inherits it via version.workspace = true
current_version=$(awk '
  /^\[/ { in_pkg = ($0 == "[workspace.package]") }
  in_pkg && /^version = "/ { gsub(/^version = "|"$/, ""); print; exit }
' "$cargo_toml")

if [[ -z "$current_version" ]]; then
  echo "error: could not read workspace.package.version from $cargo_toml" >&2
  exit 1
fi

if [[ ! "$current_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: unexpected version format '$current_version' in $cargo_toml" >&2
  exit 1
fi

IFS='.' read -r major minor patch <<<"$current_version"
case "$bump" in
  major)
    major=$((major + 1))
    minor=0
    patch=0
    ;;
  minor)
    minor=$((minor + 1))
    patch=0
    ;;
  patch)
    patch=$((patch + 1))
    ;;
esac
new_version="${major}.${minor}.${patch}"

echo "bumping ${current_version} -> ${new_version}"

old_re=${current_version//./\\.}

# obby-proto and obby-client also pin their own version as workspace.dependencies path deps
# (Cargo needs that pin to match for `cargo publish` to resolve them), so we move them together
# with workspace.package here rather than letting them drift.
awk -v old="$current_version" -v new="$new_version" -v old_re="$old_re" '
  BEGIN { replaced_pkg = 0; replaced_deps = 0 }
  /^\[/ { in_pkg = ($0 == "[workspace.package]") }
  in_pkg && $0 == "version = \"" old "\"" {
    print "version = \"" new "\""
    replaced_pkg++
    next
  }
  (/^obby-proto = \{/ || /^obby-client = \{/) && index($0, "version = \"" old "\"") {
    line = $0
    sub("version = \"" old_re "\"", "version = \"" new "\"", line)
    print line
    replaced_deps++
    next
  }
  { print }
  END {
    if (replaced_pkg != 1) {
      print "error: expected 1 workspace.package version line, replaced " replaced_pkg > "/dev/stderr"
      exit 1
    }
    if (replaced_deps != 2) {
      print "error: expected 2 internal dependency version pins, replaced " replaced_deps > "/dev/stderr"
      exit 1
    }
  }
' "$cargo_toml" >"${cargo_toml}.new"
mv "${cargo_toml}.new" "$cargo_toml"

changed_files=("$cargo_toml" "Cargo.lock")

# a hand-authored package.json under bindings/ would have its own version field; none exists
# today (obby-wasm's pkg/package.json is generated at build time from Cargo.toml), but bump it
# too if one shows up
while IFS= read -r -d '' pkg_json; do
  if grep -q '"version"' "$pkg_json"; then
    sed -E "s/(\"version\"[[:space:]]*:[[:space:]]*\")[^\"]*(\")/\1${new_version}\2/" "$pkg_json" >"${pkg_json}.new"
    mv "${pkg_json}.new" "$pkg_json"
    changed_files+=("$pkg_json")
  fi
done < <(find bindings -name 'package.json' -print0)

# obby-python's pyproject.toml declares `dynamic = ["version"]` and gets its version from
# Cargo.toml via maturin, so there is nothing to bump there; only a static `version = "..."`
# field needs touching, in case one is ever added
while IFS= read -r -d '' pyproject; do
  if grep -qE '^version[[:space:]]*=' "$pyproject"; then
    sed -E "s/^version[[:space:]]*=[[:space:]]*\"[^\"]*\"/version = \"${new_version}\"/" "$pyproject" >"${pyproject}.new"
    mv "${pyproject}.new" "$pyproject"
    changed_files+=("$pyproject")
  fi
done < <(find bindings -name 'pyproject.toml' -print0)

# the Dart package has a hand-written version, since pub.dev has no equivalent of maturin
# reading it back out of Cargo.toml
while IFS= read -r -d '' pubspec; do
  if grep -qE '^version:[[:space:]]' "$pubspec"; then
    sed -E "s/^version:[[:space:]]*.*/version: ${new_version}/" "$pubspec" >"${pubspec}.new"
    mv "${pubspec}.new" "$pubspec"
    changed_files+=("$pubspec")
  fi
done < <(find bindings -name 'pubspec.yaml' -print0)

# pub.dev looks for the version in the changelog, and writing what changed stays a human's job
changelog="bindings/obby-dart/CHANGELOG.md"
if ! grep -q "^## ${new_version}\$" "$changelog"; then
  printf '## %s\n\n' "$new_version" | cat - "$changelog" >"${changelog}.new"
  mv "${changelog}.new" "$changelog"
  changed_files+=("$changelog")
fi

echo "running the local checks before committing anything"
make ci

git add -- "${changed_files[@]}"
git commit -m "release v${new_version}"

tag="v${new_version}"
git tag -a "$tag" -m "$tag"

git push "$remote" main
git push "$remote" "$tag"

echo "released ${tag}"
