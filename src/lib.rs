//! Account sponsor: creates NEAR accounts and relays wallet-contract calls on
//! behalf of wallet connectors.
//!
//! Deployed on the sponsor account (e.g. `helper.trezu.near`) together with a
//! *public* function-call access key that is restricted to the `create_*`
//! methods and `relay_execute_signed`. Connectors (passkey, EVM, Ledger) ship
//! that key and sign calls with it, so no full-access key ever leaves the
//! operator.
//!
//! Every account is created with **zero deposit** — the contract never spends
//! its own balance on the new accounts (NEAR's zero-balance accounts cover
//! small accounts). Relays attach exactly 1 yoctoNEAR, which the wallet
//! contract's `w_execute_signed` requires. What a public key can abuse is
//! therefore limited to gas, which the access key's allowance caps, and to the
//! number of accounts and relays, which rate limits cap. Deterministic and
//! universal accounts may only run contract code from an owner-managed
//! allowlist.
//!
//! Admin methods require the owner as predecessor *and* exactly 1 yoctoNEAR
//! attached. Function-call access keys cannot attach a deposit, so the public
//! key can never call them, even if its method list were misconfigured.

use near_sdk::json_types::U64;
use near_sdk::state_init::StateInit;
use near_sdk::universal_state_init::{RawStateInit, UniversalStateInit};
use near_sdk::{
    assert_one_yocto, env, near, require, serde_json, AccountId, Gas, GasWeight, GlobalContractId,
    NearToken, Promise, PublicKey,
};

/// Gas forwarded to `<registrar>.create_account`. The mainnet `near`
/// registrar creates the account and then runs its own `on_account_created`
/// callback (~20 TGas prepaid), so callers should attach about 50 TGas in total.
pub const CREATE_NAMED_ACCOUNT_GAS: Gas = Gas::from_tgas(30);

const EVENT_STANDARD: &str = "account_sponsor";
const EVENT_VERSION: &str = "1.0.0";

/// A fixed-length rate-limit window: how many operations started in it.
#[near(serializers = [borsh])]
#[derive(Default)]
pub struct Window {
    started_at_ns: u64,
    used: u32,
}

impl Window {
    /// (window start, operations in it) as of the current block.
    fn current(&self, window_ns: u64) -> (u64, u32) {
        let now = env::block_timestamp();
        if now >= self.started_at_ns.saturating_add(window_ns) {
            (now, 0)
        } else {
            (self.started_at_ns, self.used)
        }
    }

    fn remaining(&self, window_ns: u64, max: u32) -> u32 {
        max.saturating_sub(self.current(window_ns).1)
    }

    fn take(&mut self, window_ns: u64, max: u32, error: &str) {
        let (started_at_ns, used) = self.current(window_ns);
        require!(used < max, error);
        self.started_at_ns = started_at_ns;
        self.used = used + 1;
    }
}

#[near(contract_state)]
pub struct Contract {
    /// Account allowed to change the configuration (with 1 yoctoNEAR attached).
    owner_id: AccountId,
    /// Account whose `create_account` method creates named accounts
    /// (`near` on mainnet, `testnet` on testnet). Only direct sub-accounts
    /// of it (`<name>.<registrar>`) can be requested.
    registrar_id: AccountId,
    /// Global contract code that deterministic and universal accounts may use.
    allowed_codes: Vec<GlobalContractId>,
    /// Length of both rate-limit windows.
    window_ns: u64,
    /// At most this many accounts per window (all kinds together).
    max_accounts_per_window: u32,
    accounts: Window,
    /// At most this many `relay_execute_signed` calls per window.
    max_relays_per_window: u32,
    relays: Window,
}

/// State init for a wallet account that may not exist yet, sent along with a
/// relayed call (JSON: `{"deterministic": <StateInit>}` or
/// `{"universal": "<base64 borsh>"}`).
#[near(serializers = [json])]
#[serde(rename_all = "snake_case")]
pub enum WalletStateInit {
    /// NEP-616 `0s…` account.
    Deterministic(StateInit),
    /// NEP-655 `0u…` account, forwarded byte for byte.
    Universal(RawStateInit),
}

/// Configuration and rate-limit state, as returned by [`Contract::get_config`].
#[near(serializers = [json])]
pub struct Config {
    pub owner_id: AccountId,
    pub registrar_id: AccountId,
    pub allowed_codes: Vec<GlobalContractId>,
    pub window_secs: U64,
    pub max_accounts_per_window: u32,
    /// Accounts still available in the current window.
    pub remaining_accounts_in_window: u32,
    pub max_relays_per_window: u32,
    /// Relays still available in the current window.
    pub remaining_relays_in_window: u32,
}

#[near]
impl Contract {
    #[init]
    #[private]
    pub fn new(
        owner_id: AccountId,
        registrar_id: AccountId,
        allowed_codes: Vec<GlobalContractId>,
        window_secs: U64,
        max_accounts_per_window: u32,
        max_relays_per_window: u32,
    ) -> Self {
        require!(window_secs.0 > 0, "window_secs must be positive");
        Self {
            owner_id,
            registrar_id,
            allowed_codes,
            window_ns: secs_to_ns(window_secs.0),
            max_accounts_per_window,
            accounts: Window::default(),
            max_relays_per_window,
            relays: Window::default(),
        }
    }

    // ─── Account creation (public key) ──────────────────────────────────────

    /// Creates `<name>.<registrar>` with `public_key` as its full-access key by
    /// calling `<registrar>.create_account` (no deposit). Resolves to the
    /// registrar's result.
    pub fn create_named_account(
        &mut self,
        account_id: AccountId,
        public_key: PublicKey,
    ) -> Promise {
        require!(
            account_id.get_parent_account_id() == Some(self.registrar_id.as_ref()),
            format!(
                "Only direct sub-accounts of {} can be created",
                self.registrar_id
            )
        );
        self.take_account();
        emit("account_requested", "named", &account_id);
        Promise::new(self.registrar_id.clone()).function_call(
            "create_account".to_string(),
            serde_json::json!({ "new_account_id": account_id, "new_public_key": public_key })
                .to_string()
                .into_bytes(),
            NearToken::from_yoctonear(0),
            CREATE_NAMED_ACCOUNT_GAS,
        )
    }

    /// Creates a NEAR-implicit (64 hex) or ETH-implicit (`0x` + 40 hex)
    /// account by sending it 0 NEAR. The account is controlled by the key
    /// (or Ethereum address) its id is derived from.
    pub fn create_implicit_account(&mut self, account_id: AccountId) -> Promise {
        require!(
            is_implicit(&account_id),
            "Expected a NEAR-implicit (64 hex) or ETH-implicit (0x + 40 hex) account id"
        );
        self.take_account();
        emit("account_requested", "implicit", &account_id);
        Promise::new(account_id).transfer(NearToken::from_yoctonear(0))
    }

    /// Creates the NEP-616 deterministic (`0s…`) account for `state_init`
    /// (no deposit). The code must be on the allowlist.
    pub fn create_deterministic_account(&mut self, state_init: StateInit) -> Promise {
        let account_id = self.check_deterministic(&state_init);
        self.take_account();
        emit("account_requested", "deterministic", &account_id);
        Promise::new(account_id).state_init(state_init, NearToken::from_yoctonear(0))
    }

    /// Creates the NEP-655 universal (`0u…`) account for the borsh
    /// `state_init` (base64 in JSON), forwarded byte for byte (no deposit).
    /// A key-only account (no code) is allowed; contract code must be on the
    /// allowlist. An account with neither code nor keys is refused, as nobody
    /// could ever control it.
    pub fn create_universal_account(&mut self, state_init: RawStateInit) -> Promise {
        let (account_id, decoded) = decode_universal(&state_init);
        match decoded.code() {
            Some(code) => self.require_allowed_code(code),
            None => require!(
                !decoded.access_keys().is_empty(),
                "A universal account needs contract code or at least one access key"
            ),
        }
        self.take_account();
        emit("account_requested", "universal", &account_id);
        Promise::new(account_id).universal_state_init(state_init, NearToken::from_yoctonear(0))
    }

    // ─── Wallet-contract relay (public key) ─────────────────────────────────

    /// Calls `w_execute_signed({ msg, proof })` on a deterministic (`0s…`) or
    /// universal (`0u…`) wallet account with 1 yoctoNEAR (paid by this
    /// contract) and all remaining gas. With `state_init`, the wallet account
    /// is initialized in the same receipt first; it must derive to
    /// `wallet_id` and run allowlisted code.
    ///
    /// `msg` and `proof` are forwarded as they are: the wallet contract
    /// verifies the signature. Attach enough gas for the wallet's work (the
    /// connectors attach 300 TGas).
    pub fn relay_execute_signed(
        &mut self,
        wallet_id: AccountId,
        msg: serde_json::Value,
        proof: String,
        state_init: Option<WalletStateInit>,
    ) -> Promise {
        require!(
            is_wallet_account(&wallet_id),
            "Relays only go to deterministic (0s…) or universal (0u…) wallet accounts"
        );
        let mut promise = Promise::new(wallet_id.clone());
        match state_init {
            None => {}
            Some(WalletStateInit::Deterministic(state_init)) => {
                let derived = self.check_deterministic(&state_init);
                require!(
                    derived == wallet_id,
                    "state_init does not derive to wallet_id"
                );
                promise = promise.state_init(state_init, NearToken::from_yoctonear(0));
            }
            Some(WalletStateInit::Universal(state_init)) => {
                let (derived, decoded) = decode_universal(&state_init);
                require!(
                    derived == wallet_id,
                    "state_init does not derive to wallet_id"
                );
                let code = decoded.code().unwrap_or_else(|| {
                    env::panic_str("A wallet's universal state init needs contract code")
                });
                self.require_allowed_code(code);
                promise = promise.universal_state_init(state_init, NearToken::from_yoctonear(0));
            }
        }
        self.relays.take(
            self.window_ns,
            self.max_relays_per_window,
            "Too many relays recently; try again later",
        );
        emit("relay_requested", "w_execute_signed", &wallet_id);
        promise.function_call_weight(
            "w_execute_signed".to_string(),
            serde_json::json!({ "msg": msg, "proof": proof })
                .to_string()
                .into_bytes(),
            NearToken::from_yoctonear(1),
            Gas::from_gas(0),
            GasWeight(1),
        )
    }

    // ─── Views ──────────────────────────────────────────────────────────────

    pub fn get_config(&self) -> Config {
        Config {
            owner_id: self.owner_id.clone(),
            registrar_id: self.registrar_id.clone(),
            allowed_codes: self.allowed_codes.clone(),
            window_secs: U64(self.window_ns / 1_000_000_000),
            max_accounts_per_window: self.max_accounts_per_window,
            remaining_accounts_in_window: self
                .accounts
                .remaining(self.window_ns, self.max_accounts_per_window),
            max_relays_per_window: self.max_relays_per_window,
            remaining_relays_in_window: self
                .relays
                .remaining(self.window_ns, self.max_relays_per_window),
        }
    }

    // ─── Admin (owner, 1 yoctoNEAR) ─────────────────────────────────────────

    #[payable]
    pub fn add_allowed_code(&mut self, code: GlobalContractId) {
        self.assert_owner();
        if !self.allowed_codes.contains(&code) {
            self.allowed_codes.push(code);
        }
    }

    #[payable]
    pub fn remove_allowed_code(&mut self, code: GlobalContractId) {
        self.assert_owner();
        self.allowed_codes.retain(|c| c != &code);
    }

    #[payable]
    pub fn set_rate_limits(
        &mut self,
        window_secs: U64,
        max_accounts_per_window: u32,
        max_relays_per_window: u32,
    ) {
        self.assert_owner();
        require!(window_secs.0 > 0, "window_secs must be positive");
        self.window_ns = secs_to_ns(window_secs.0);
        self.max_accounts_per_window = max_accounts_per_window;
        self.max_relays_per_window = max_relays_per_window;
    }

    #[payable]
    pub fn set_owner(&mut self, owner_id: AccountId) {
        self.assert_owner();
        self.owner_id = owner_id;
    }
}

impl Contract {
    fn assert_owner(&self) {
        assert_one_yocto();
        require!(
            env::predecessor_account_id() == self.owner_id,
            "Only the owner can do this"
        );
    }

    fn require_allowed_code(&self, code: &GlobalContractId) {
        require!(
            self.allowed_codes.contains(code),
            "This contract code is not on the sponsor's allowlist"
        );
    }

    /// Allowlisted code check; returns the derived `0s…` account id.
    fn check_deterministic(&self, state_init: &StateInit) -> AccountId {
        let StateInit::V1(v1) = state_init;
        self.require_allowed_code(&v1.code);
        state_init.derive_account_id()
    }

    fn take_account(&mut self) {
        self.accounts.take(
            self.window_ns,
            self.max_accounts_per_window,
            "Too many accounts created recently; try again later",
        );
    }
}

/// Validates (canonical encoding included) and decodes a universal state
/// init; returns its `0u…` account id.
fn decode_universal(state_init: &RawStateInit) -> (AccountId, UniversalStateInit) {
    let account_id = state_init
        .try_derive_account_id()
        .unwrap_or_else(|e| env::panic_str(&format!("Invalid universal state init: {e:?}")));
    let decoded = UniversalStateInit::try_from(state_init)
        .unwrap_or_else(|_| env::panic_str("Invalid universal state init"));
    (account_id, decoded)
}

fn is_lower_hex(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_implicit(account_id: &AccountId) -> bool {
    let id = account_id.as_str();
    (id.len() == 64 && is_lower_hex(id))
        || (id.len() == 42 && id.starts_with("0x") && is_lower_hex(&id[2..]))
}

/// `0s` + 40 hex (NEP-616) or `0u` + 52 base32 characters (NEP-655).
fn is_wallet_account(account_id: &AccountId) -> bool {
    let id = account_id.as_str();
    (id.len() == 42 && id.starts_with("0s") && is_lower_hex(&id[2..]))
        || (id.len() == 54
            && id.starts_with("0u")
            && id[2..].bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn secs_to_ns(secs: u64) -> u64 {
    secs.saturating_mul(1_000_000_000)
}

fn emit(event: &str, kind: &str, account_id: &AccountId) {
    let event = serde_json::json!({
        "standard": EVENT_STANDARD,
        "version": EVENT_VERSION,
        "event": event,
        "data": [{ "kind": kind, "account_id": account_id }],
    });
    env::log_str(&format!("EVENT_JSON:{event}"));
}

#[cfg(test)]
mod tests;
