#!/usr/bin/env bash
# Enforce the repository source layout:
#   <scope>/[<subscope>/]<atom>.rs
# Directories name scopes, files are single lowercase words, and migrated
# crate roots only hold lib.rs/main.rs. Cargo target paths keep their public
# names even when those names are not Rust module atoms.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

violations=0
report() {
  printf 'layout: %s\n' "$*" >&2
  violations=$((violations + 1))
}

is_target_exception() {
  case "$1" in
    crates/atmospheric/examples/pds_check.rs | crates/atmospheric/src/bin/atmospheric-worker.rs)
      return 0
      ;;
  esac
  return 1
}

# Every tracked or new Rust file must use an atom filename. The protocol crate
# is still being migrated, so its flat modules are allowed for now, but a new
# multi-word name is not.
while IFS= read -r -d '' file; do
  if is_target_exception "$file"; then
    continue
  fi
  base="${file##*/}"
  if [[ ! "$base" =~ ^[a-z][a-z0-9]*\.rs$ ]]; then
    report "$file: Rust file names must be a single lowercase word plus .rs"
  fi
done < <(git ls-files --cached --others --exclude-standard -z -- '*.rs')

# Directory components below source/test/example roots are scopes.
scope_roots=(
  "crates/atmospheric/src"
  "crates/atmospheric/tests"
  "crates/atmospheric/examples"
  "crates/atmospheric-server/src"
  "crates/atmospheric-server/tests"
  "mixins"
)
for root in "${scope_roots[@]}"; do
  if [[ ! -d "$root" ]]; then
    continue
  fi
  while IFS= read -r -d '' dir; do
    base="${dir##*/}"
    if [[ ! "$base" =~ ^[a-z][a-z0-9]*$ ]]; then
      report "$dir/: scope directory names must be a single lowercase word"
    fi
  done < <(find "$root" -type d -print0)
done

# The server crate is fully migrated: implementation atoms live in scopes,
# and each scope directory has its mod.rs declaration.
server_root="crates/atmospheric-server/src"
if [[ -d "$server_root" ]]; then
  while IFS= read -r -d '' file; do
    rel="${file#"$server_root"/}"
    case "$rel" in
      lib.rs | main.rs)
        continue
        ;;
      */*)
        continue
        ;;
    esac
    report "$file: crate-root modules must live in a scope directory"
  done < <(git ls-files --cached --others --exclude-standard -z -- "$server_root")

  while IFS= read -r -d '' dir; do
    if [[ "$dir" == "$server_root" ]]; then
      continue
    fi
    if [[ ! -f "$dir/mod.rs" ]]; then
      report "$dir/: every source scope needs a mod.rs"
    fi
  done < <(find "$server_root" -mindepth 1 -type d -print0)
fi

if ((violations > 0)); then
  printf 'layout: %d violation(s); see the source layout rules in AGENTS.md\n' "$violations" >&2
  exit 1
fi
printf 'layout: scope/atom naming and placement are clean\n'
