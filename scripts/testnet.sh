#!/usr/bin/env bash
# Optional NEAR testnet deploy and demo.
# This script NEVER talks to mainnet. It refuses to run if NEAR_ENV/NEAR_NETWORK
# is set to mainnet, and it does not create accounts or spend keys on its own.
#
# Prerequisites (not installed in every environment):
#   - NEAR CLI: https://github.com/near/near-cli-rs  (or `npm i -g near-cli`)
#   - A testnet account funded by a faucet (https://near-faucet.io or `near account create-account`)
#   - A compiled WASM from scripts/build-wasm.sh
#   - Environment variables (see .env.example): NEAR_CONTRACT, NEAR_OWNER, NEAR_AGENT
#
# This script prints the exact commands and, if a CLI is present and
# SPENDGUARD_EXECUTE=1, runs the view/simulate ones. It will not sign
# transactions unless you also set SPENDGUARD_SIGN=1 after reviewing them.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

network="${NEAR_NETWORK:-${NEAR_ENV:-testnet}}"
if [[ "$network" != "testnet" ]]; then
  echo "error: refusing to run — NEAR_NETWORK/NEAR_ENV is '$network', not 'testnet'." >&2
  exit 1
fi

if [[ -f .env ]]; then
  # shellcheck disable=SC1091
  set -a; source .env; set +a
fi

CONTRACT="${NEAR_CONTRACT:-}"
OWNER="${NEAR_OWNER:-}"
AGENT="${NEAR_AGENT:-}"
WASM="${SPENDGUARD_WASM:-target/wasm32-unknown-unknown/release/spendguard.wasm}"

echo "Network : $network   (mainnet is refused)"
echo "Contract: ${CONTRACT:-<set NEAR_CONTRACT>}"
echo "Owner   : ${OWNER:-<set NEAR_OWNER>}"
echo "Agent   : ${AGENT:-<set NEAR_AGENT>}"
echo "WASM    : $WASM"
echo

print_commands() {
  cat <<CMDS
  # 2. Build the contract (requires wasm32-unknown-unknown std):
  ./scripts/build-wasm.sh

  # 3. Deploy and initialize (fail-closed: agent cannot spend yet):
  near contract deploy ${CONTRACT:-CONTRACT} use-file ${WASM} \
    without-init-call network-config testnet sign-with-keychain send
  near contract call-function as-transaction ${CONTRACT:-CONTRACT} new \
    json-args '{"owner":"${OWNER:-OWNER}","agent":"${AGENT:-AGENT}"}' \
    prepaid-gas '30.0 Tgas' attached-deposit '0 NEAR' \
    sign-as ${OWNER:-OWNER} network-config testnet sign-with-keychain send

  # 4. Owner deposits 5 NEAR and sets the demo policy (1 NEAR / tx, 2 NEAR / day):
  near contract call-function as-transaction ${CONTRACT:-CONTRACT} deposit \
    json-args '{}' prepaid-gas '30.0 Tgas' attached-deposit '5 NEAR' \
    sign-as ${OWNER:-OWNER} network-config testnet sign-with-keychain send
  near contract call-function as-transaction ${CONTRACT:-CONTRACT} set_policy \
    json-args '{"per_tx_max":"1000000000000000000000000","daily_max":"2000000000000000000000000","allowed_receivers":["${OWNER:-OWNER}"],"allowed_methods":["api_charge"]}' \
    prepaid-gas '30.0 Tgas' attached-deposit '0 NEAR' \
    sign-as ${OWNER:-OWNER} network-config testnet sign-with-keychain send

  # 5. (Recommended) Add a Function-Call access key on the AGENT account,
  #    scoped ONLY to this contract and these two methods. The agent never
  #    gets a full-access key. If the gas allowance runs out the key just
  #    stops working — it cannot transfer NEAR or add new keys.
  #    near account add-key ${AGENT:-AGENT} grant-function-call-access \
  #      --receiver-id ${CONTRACT:-CONTRACT} \
  #      --method-names agent_spend,agent_call ...

  # 6. Dry-run (view, no signature needed) then a live under-limit spend,
  #    then an over-limit spend that must fail:
  near contract call-function as-read-only ${CONTRACT:-CONTRACT} simulate \
    json-args '{"caller":"${AGENT:-AGENT}","receiver":"${OWNER:-OWNER}","method":null,"amount":"400000000000000000000000"}' \
    network-config testnet
  near contract call-function as-transaction ${CONTRACT:-CONTRACT} agent_spend \
    json-args '{"receiver":"${OWNER:-OWNER}","amount":"400000000000000000000000","memo":"api fee"}' \
    prepaid-gas '30.0 Tgas' attached-deposit '0 NEAR' \
    sign-as ${AGENT:-AGENT} network-config testnet sign-with-keychain send
  # This next one is expected to fail with PerTxLimitExceeded:
  near contract call-function as-transaction ${CONTRACT:-CONTRACT} agent_spend \
    json-args '{"receiver":"${OWNER:-OWNER}","amount":"1500000000000000000000000","memo":"too much"}' \
    prepaid-gas '30.0 Tgas' attached-deposit '0 NEAR' \
    sign-as ${AGENT:-AGENT} network-config testnet sign-with-keychain send

  # 7. Confirm daily usage:
  near contract call-function as-read-only ${CONTRACT:-CONTRACT} get_daily_usage \
    json-args '{}' network-config testnet
  near contract call-function as-read-only ${CONTRACT:-CONTRACT} get_policy \
    json-args '{}' network-config testnet
CMDS
}

if ! command -v near >/dev/null 2>&1; then
  echo "NEAR CLI is not on PATH. Install it, then re-run. Suggested commands:"
  echo
  echo "  # 1. Create/fund a testnet account (faucet, not mainnet):"
  echo "  #    https://near-faucet.io    or    https://docs.near.org/tools/near-cli"
  echo
  print_commands
  exit 0
fi

echo "NEAR CLI found: $(command -v near)"
echo "Exact commands (not executed unless SPENDGUARD_SIGN=1):"
echo
print_commands
echo

if [[ "${SPENDGUARD_SIGN:-}" == "1" ]]; then
  echo "SPENDGUARD_SIGN=1 is set, but this script still will not sign for you."
  echo "Copy the commands above and run them yourself after reviewing accounts."
  echo "This is intentional: no unattended testnet signatures."
fi
