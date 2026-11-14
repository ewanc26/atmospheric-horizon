#!/usr/bin/env bash
# Exercise scripts/check-layout.sh against an isolated git fixture.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
guard="$repo_root/scripts/check-layout.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

git -C "$fixture" init --quiet
mkdir -p "$fixture/scripts"
cp "$guard" "$fixture/scripts/check-layout.sh"

make_file() {
  local path="$fixture/$1"
  mkdir -p "$(dirname "$path")"
  : >"$path"
}

# A valid tree with both migrated crates, the Cargo target exceptions and the
# mixin helper scope.
for path in \
  crates/atmospheric/src/lib.rs \
  crates/atmospheric/src/account/mod.rs \
  crates/atmospheric/src/account/identity.rs \
  crates/atmospheric/src/bin/atmospheric-worker.rs \
  crates/atmospheric/src/social/mod.rs \
  crates/atmospheric-server/src/lib.rs \
  crates/atmospheric-server/src/api/mod.rs \
  crates/atmospheric-server/src/api/router.rs \
  crates/atmospheric/examples/pds_check.rs \
  mixins/atmospheric/runtime.rs \
  mixins/atmospheric/colibri/join.rs; do
  make_file "$path"
done

check_passes() {
  if ! (cd "$fixture" && bash scripts/check-layout.sh >/dev/null); then
    echo "layout test: valid fixture was rejected" >&2
    exit 1
  fi
}

expect_failure() {
  local label="$1"
  local expected="$2"
  local log="$fixture/layout.log"
  if (cd "$fixture" && bash scripts/check-layout.sh >"$log" 2>&1); then
    echo "layout test: $label was accepted" >&2
    exit 1
  fi
  if ! grep -Fq -- "$expected" "$log"; then
    echo "layout test: $label did not report: $expected" >&2
    cat "$log" >&2
    exit 1
  fi
  printf 'ok: %s\n' "$label"
}

check_passes

make_file crates/atmospheric/src/flat.rs
expect_failure "flat protocol crate root" \
  "crates/atmospheric/src/flat.rs: crate-root modules must live in a scope directory"
rm -f "$fixture/crates/atmospheric/src/flat.rs"

make_file crates/atmospheric/src/social/bad_name.rs
expect_failure "multi-word atom" \
  "crates/atmospheric/src/social/bad_name.rs: Rust file names must be a single lowercase word plus .rs"
rm -f "$fixture/crates/atmospheric/src/social/bad_name.rs"

make_file crates/atmospheric/src/social/bad_scope/mod.rs
make_file crates/atmospheric/src/social/bad_scope/atom.rs
expect_failure "multi-word scope" \
  "crates/atmospheric/src/social/bad_scope/: scope directory names must be a single lowercase word"
rm -rf "$fixture/crates/atmospheric/src/social/bad_scope"

make_file crates/atmospheric/src/social/missing/atom.rs
expect_failure "missing scope mod.rs" \
  "crates/atmospheric/src/social/missing/: every source scope needs a mod.rs"
rm -rf "$fixture/crates/atmospheric/src/social/missing"

make_file crates/atmospheric-server/src/flat.rs
expect_failure "flat server crate root" \
  "crates/atmospheric-server/src/flat.rs: crate-root modules must live in a scope directory"
rm -f "$fixture/crates/atmospheric-server/src/flat.rs"

check_passes
printf 'layout tests: valid fixture and 5 violations handled\n'
