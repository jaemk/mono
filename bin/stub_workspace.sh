#!/usr/bin/env bash
# stub_workspace.sh
#
# Reads the workspace Cargo.toml, finds all member crates, and creates minimal
# stub source files for each so that `cargo build` can compile and cache all
# third-party dependencies without requiring the real source code.
#
# Usage: ./bin/stub_workspace.sh [workspace-root]
#   workspace-root defaults to the current directory.

set -euo pipefail

ROOT="${1:-.}"
WORKSPACE_TOML="$ROOT/Cargo.toml"

if [[ ! -f "$WORKSPACE_TOML" ]]; then
  echo "Error: $WORKSPACE_TOML not found" >&2
  exit 1
fi

# Extract member paths from the [workspace] members array.
# Handles one path per line inside members = [ ... ].
# Uses only POSIX awk/sed so it works on both Linux and macOS.
members=$(awk '
  /^\[workspace\]/ { in_ws=1 }
  in_ws && /members[ \t]*=[ \t]*\[/ { in_members=1 }
  in_members {
    # print everything between double-quotes on this line
    line=$0
    while (match(line, /"[^"]+"/) > 0) {
      val=substr(line, RSTART+1, RLENGTH-2)
      print val
      line=substr(line, RSTART+RLENGTH)
    }
  }
  in_members && /\]/ { in_members=0; in_ws=0 }
' "$WORKSPACE_TOML")

if [[ -z "$members" ]]; then
  echo "Error: no workspace members found in $WORKSPACE_TOML" >&2
  exit 1
fi

for member in $members; do
  crate_dir="$ROOT/$member"
  src_dir="$crate_dir/src"

  if [[ ! -f "$crate_dir/Cargo.toml" ]]; then
    echo "Warning: $crate_dir/Cargo.toml not found, skipping" >&2
    continue
  fi

  mkdir -p "$src_dir"

  # Determine targets from the real Cargo.toml:
  #   [[bin]] sections → need src/main.rs (or the explicit path)
  #   [lib] section or absence of any [[bin]] → need src/lib.rs
  has_lib=false
  has_bin=false

  if grep -q '^\[\[bin\]\]' "$crate_dir/Cargo.toml"; then
    has_bin=true
  fi

  if grep -q '^\[lib\]' "$crate_dir/Cargo.toml"; then
    has_lib=true
  fi

  # Cargo defaults: if neither [lib] nor [[bin]] is declared, a crate with
  # src/main.rs becomes a binary and one with src/lib.rs becomes a library.
  # In Docker the src/ tree doesn't exist yet when this script runs, so we
  # can't rely on the filesystem — create both stubs as a safe fallback.
  if [[ "$has_lib" == false && "$has_bin" == false ]]; then
    has_lib=true
    has_bin=true
  fi

  if [[ "$has_lib" == true ]]; then
    echo "// stub" > "$src_dir/lib.rs"
    echo "  Stubbed $member/src/lib.rs"
  fi

  if [[ "$has_bin" == true ]]; then
    printf 'fn main() {}\n' > "$src_dir/main.rs"
    echo "  Stubbed $member/src/main.rs"
  fi
done

echo "Done."

