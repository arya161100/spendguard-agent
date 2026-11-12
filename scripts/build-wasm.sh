#!/usr/bin/env bash
# Compile the SpendGuard contract to WASM for a later testnet deploy.
# Requires the wasm32-unknown-unknown rustc target (rustup target add ...).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Prefer rustup (has wasm32-unknown-unknown after `rustup target add`).
if command -v rustup >/dev/null 2>&1 && rustup show active-toolchain >/dev/null 2>&1; then
  export PATH="${HOME}/.cargo/bin:${PATH}"
  unset RUSTC CARGO
elif [[ -x /usr/bin/cargo && -x /usr/bin/rustc ]]; then
  export PATH="/usr/bin:${PATH}"
  export RUSTC="${RUSTC:-/usr/bin/rustc}"
  export CARGO="${CARGO:-/usr/bin/cargo}"
fi

TARGET="wasm32-unknown-unknown"
if ! rustc --print target-list | grep -qx "$TARGET"; then
  echo "error: rustc does not know about $TARGET" >&2
  exit 1
fi

# rustc listing the target is not the same as having its standard library.
if ! rustc --print sysroot --target "$TARGET" >/dev/null 2>&1 \
   || [[ ! -d "$(rustc --print sysroot)/lib/rustlib/${TARGET}" ]]; then
  echo "error: the $TARGET standard library is not installed." >&2
  echo "Install it and re-run, for example:" >&2
  echo "  rustup target add ${TARGET}" >&2
  echo "Then:  $0" >&2
  exit 1
fi

export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-s"
cargo build --target "$TARGET" --release -p spendguard

OUT="target/${TARGET}/release/spendguard.wasm"
if [[ ! -f "$OUT" ]]; then
  # crate-type cdylib may be named with underscores or hyphens depending on cargo.
  OUT="$(ls -1 target/${TARGET}/release/*.wasm | head -n 1)"
fi
ls -la "$OUT"
echo "WASM ready: $OUT"
echo "Deploy is a separate, testnet-only step: see scripts/testnet.sh"
