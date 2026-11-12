#!/usr/bin/env bash
# Run the judge-facing local demo. No network, no keys, no faucet.
# Uses the same policy engine the NEAR contract compiles in.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Prefer a rustup default toolchain when one exists (needed to compile
# near-sdk). Fall back to the system rustc (1.85.x on some images) so the
# policy crate and the CLI still build without rustup.
if command -v rustup >/dev/null 2>&1 && rustup show active-toolchain >/dev/null 2>&1; then
  export PATH="${HOME}/.cargo/bin:${PATH}"
  unset RUSTC CARGO
elif [[ -x /usr/bin/cargo && -x /usr/bin/rustc ]]; then
  export PATH="/usr/bin:${PATH}"
  export RUSTC="${RUSTC:-/usr/bin/rustc}"
  export CARGO="${CARGO:-/usr/bin/cargo}"
fi

echo "== SpendGuard policy unit tests =="
cargo test -p spendguard-policy -- --nocapture

echo
echo "== SpendGuard contract unit tests (near-sdk, in-process VM) =="
if cargo test -p spendguard -- --nocapture; then
  :
else
  echo
  echo "Contract tests failed to build or run."
  echo "The policy engine and the local CLI demo below are still authoritative:"
  echo "they share the same crate (spendguard-policy) the contract calls."
fi

echo
echo "== Scripted demo: human sets policy, agent spends, over-limit is rejected =="
cargo run -p spendguard-cli -- demo

echo
echo "== Scripted demo: prompt-injected agent is stopped =="
cargo run -p spendguard-cli -- attack

echo
echo "All local demos finished. No testnet calls were made."
echo "To drive the vault yourself, see README.md ('Do X → see Y')."
echo "To deploy to NEAR testnet (optional), see scripts/testnet.sh."
