# near-account-sponsor

A small NEAR contract that creates accounts and relays wallet-contract calls
on behalf of wallet connectors (passkey, EVM/EIP-712, Ledger), so they no
longer need a full-access key of the sponsor account.

It is deployed on the sponsor account (`helper.trezu.near`) and called through
a **public function-call access key** that only allows the four `create_*`
methods and `relay_execute_signed`. That key can be shipped in client code: everything it can do is
listed below, and all of it is bounded.

| Method | Creates | How |
| --- | --- | --- |
| `create_named_account(account_id, public_key)` | `<name>.near` with `public_key` as full-access key | calls `near.create_account({ new_account_id, new_public_key })` with no deposit (as in [this mainnet tx](https://nearblocks.io/txns/APy8xaF1LM5pbZD3ny6takYwMCDnPCs3BGzvhXwjrkwm)) |
| `create_implicit_account(account_id)` | NEAR-implicit (64 hex) or ETH-implicit (`0x` + 40 hex) | `Transfer` of 0 NEAR |
| `create_deterministic_account(state_init)` | NEP-616 deterministic account `0s…` | `DeterministicStateInit` action, no deposit |
| `create_universal_account(state_init)` | NEP-655 universal account `0u…` | `UniversalStateInit` action with the given bytes, no deposit |

All four return the promise, so a failed creation fails the transaction.

### Relaying `w_execute_signed`

`relay_execute_signed(state_init, args)` calls `w_execute_signed` on a wallet
account with **1 yoctoNEAR** (paid by the contract, as the wallet contract
requires) and all remaining gas. It replaces the transactions the passkey and
EIP-712 connectors used to sign with the sponsor's full-access key
(`[StateInit?, FunctionCall w_execute_signed]` sent to the wallet account).

- `state_init` is **required**: `{"deterministic": <StateInit>}` or
  `{"universal": "<base64>"}`. The wallet account id is derived from it, and
  its code must be on the allowlist (a universal state init must carry code),
  so a relay can only ever reach a wallet running allowlisted code. The state
  init is attached to every relay: it creates the wallet if needed and is a
  no-op on an existing wallet (its storage is left alone; the sandbox tests
  check this).
- `args` is the base64 of the `w_execute_signed` JSON arguments
  (`{"msg": …, "proof": …}`), forwarded byte for byte, so the signed message
  reaches the wallet exactly as the client built it. The wallet contract
  checks the signature.
- Attach enough gas for the wallet's own work (the connectors use 300 TGas).
  The promise resolves to the wallet's result.

NEP-366 delegate actions keep working: the public key can sign a delegate
action whose only action is a `FunctionCall` to `relay_execute_signed` on the
sponsor account with 0 deposit, which a relayer then submits.

## Limits

- **No deposits.** Every account is created with 0 NEAR (zero-balance
  accounts). The contract never spends its own balance on new accounts; each
  relay costs it 1 yoctoNEAR.
- **Gas.** Paid from the sponsor account's balance, like any transaction it
  signs. The public key's allowance caps how much gas it can ever spend; when
  it runs out, the operator adds a new key or a larger allowance.
- **Named accounts** must be direct sub-accounts of the registrar
  (`alice.near`, not `team.alice.near` or `alice.tg`).
- **Code allowlist.** Deterministic accounts, and universal accounts with code,
  may only use global contracts on an owner-managed allowlist. Key-only
  universal accounts are allowed; ones with neither code nor keys are refused.
- **Rate limits.** Per `window_secs`: at most `max_accounts_per_window`
  accounts (all kinds together) and, separately, at most
  `max_relays_per_window` relays.
- **Own keys only.** The public methods only accept calls signed with the
  sponsor account's own keys (predecessor = the sponsor), so other accounts and
  contracts can't use the sponsor without going through the key's allowance.
- **Admin methods** (`add_allowed_code`, `remove_allowed_code`,
  `set_rate_limits`, `propose_owner`) require the owner as predecessor **and**
  exactly 1 yoctoNEAR. Function-call keys can't attach deposits, so the public
  key can't call them even if its method list were wrong. Ownership moves in
  two steps: `propose_owner`, then `accept_owner` from the new owner (also with
  1 yoctoNEAR).
- **Pausing.** `set_rate_limits` with a maximum of 0 stops that kind of
  operation until it is raised again.

Each creation logs an NEP-297 event (relays log `relay_requested`; admin
changes log `allowed_code_added`, `allowed_code_removed`, `rate_limits_set`,
`owner_proposed` and `owner_changed`):

```
EVENT_JSON:{"standard":"account_sponsor","version":"1.0.0","event":"account_requested","data":[{"kind":"named","account_id":"alice.near"}]}
```

## Calling it

Sign a transaction from `helper.trezu.near` with the public key, receiver
`helper.trezu.near`, one `FunctionCall` action, 0 deposit:

```jsonc
// create_named_account — attach ~50 TGas (30 TGas go to near.create_account)
{ "account_id": "alice.near", "public_key": "ed25519:…" }

// create_implicit_account
{ "account_id": "4a1f…(64 hex)" }

// create_deterministic_account — StateInit JSON; data keys/values base64
{ "state_init": { "V1": { "code": { "account_id": "eip712-wallet-contract.trezu.near" }, "data": { "": "<base64 wallet state>" } } } }

// create_universal_account — borsh UniversalStateInit, base64; forwarded byte for byte
{ "state_init": "<base64>" }

// relay_execute_signed — attach 300 TGas; args = base64('{"msg":…,"proof":…}')
{ "state_init": { "deterministic": { "V1": { … } } }, "args": "<base64>" }
```

Each relay logs a `relay_requested` event with the wallet account id. Sign
these transactions from the sponsor account with the public key: calls from any
other account are refused.

`get_config()` returns the owner, registrar, allowlist, limits and how many
accounts and relays are left in the current window.

## Deploy

Build (requires [cargo-near](https://github.com/near/cargo-near)):

```bash
cargo near build non-reproducible-wasm
```

Deploy and initialize on the sponsor account with its full-access key. The
owner should be the account that keeps the full-access key (or another
operator account):

```bash
near contract deploy helper.trezu.near use-file target/near/account_sponsor.wasm \
  with-init-call new json-args '{
    "owner_id": "trezu.near",
    "registrar_id": "near",
    "allowed_codes": [
      { "account_id": "eip712-wallet-contract.trezu.near" },
      { "account_id": "0saf343be226341c0eca7dba6d0b29d49bdff3ad03" },
      { "account_id": "0sa7ed6ace79f0fd97313c465fd72a774990048501" }
    ],
    "window_secs": "3600",
    "max_accounts_per_window": 200,
    "max_relays_per_window": 2000
  }' prepaid-gas '30 Tgas' attached-deposit '0 NEAR' \
  network-config mainnet sign-with-keychain send
```

The two `0s…` ids are the passkey wallet code (`wallet-webauthn-p256` and
`wallet-webauthn-ed25519`); adjust the list to the codes your connectors use.

Then add the public key, restricted to the public methods:

```bash
near account add-key helper.trezu.near grant-function-call-access \
  --allowance '5 NEAR' \
  --contract-account-id helper.trezu.near \
  --function-names 'create_named_account, create_implicit_account, create_deterministic_account, create_universal_account, relay_execute_signed' \
  use-manually-provided-public-key ed25519:<public key> \
  network-config mainnet sign-with-keychain send
```

## Tests

```bash
cargo test --lib          # unit tests
cargo test --test sandbox # neard 2.14 sandbox; builds the contract and the registrar mock
```

The sandbox tests download neard 2.14 at build time. To use a binary you
already have, set `NEAR_SANDBOX_BIN_PATH`. The macOS build links Homebrew's
`liblzma`; without Homebrew, point `NEAR_SANDBOX_BIN_PATH` at a wrapper script
that runs the binary with `DYLD_LIBRARY_PATH=/usr/lib`.

The sandbox tests call the contract through a function-call key, create all
four kinds of accounts, relay `w_execute_signed` to new and existing
deterministic and universal wallets (checking that a repeated state init keeps
the wallet's storage), and check the refusals: deeper named accounts, codes
not on the allowlist (for creation and relays), calls from other accounts,
admin calls through function-call keys, and the rate limit. `registrar-mock/` is a test-only contract: a stand-in for the `near`
registrar that can also publish a global contract, and a `w_execute_signed`
stub for relays.

## Security notes

Audit findings that the contract addresses:

- **Relay targets run allowlisted code.** The wallet id is derived from the
  required state init, so the sponsor never calls code nobody vetted, and it
  can't be used to send 1 yoctoNEAR calls to arbitrary accounts.
- **Relayed arguments are not re-encoded.** They are forwarded byte for byte,
  so numbers beyond 64 bits, key order and string escapes survive.
- **No use from other accounts.** Public methods require predecessor = the
  sponsor account.
- **Admin is owner + 1 yoctoNEAR**, with two-step ownership transfer and
  events for every change.

Risks that remain by design, and how to handle them:

- **The public key is public.** Anyone can use up the rate limits (blocking
  onboarding until the window resets) or the key's gas allowance. Keep the
  allowance modest, watch `get_config` and the events, and add a new key or
  adjust limits when needed. Each relay to a valid wallet with an invalid
  proof still costs gas; allowlisted wallet code rejects such calls early.
- **Named accounts can be claimed for free**, within the rate limit. If that
  matters, keep `max_accounts_per_window` low or create named accounts from a
  backend instead.
- **Failed attempts count.** A rate-limit slot is taken before the creation or
  relay receipt runs, even if it later fails.
- **Allowlisting by account id trusts that account's future upgrades** (the
  account can redeploy its global code). Allowlist by code hash
  (`{"hash": "<base58>"}`) to pin exact code.
- **Upgrades.** The contract has no migration method; deploying a new version
  with a different state layout needs one.
