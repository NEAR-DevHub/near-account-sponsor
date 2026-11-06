//! Test-only helper contract.
//!
//! * Stand-in for the mainnet `near` registrar's `create_account`: creates
//!   `<name>.<this account>` with a full-access key and forwards the attached
//!   deposit (0 in the sponsor's case).
//! * `deploy_global_by_account_id`: publishes `code` as a NEP-591 global
//!   contract identified by this account, so sandbox tests have code to
//!   reference from deterministic and universal accounts.
//! * `w_execute_signed`: stub of the wallet contract's entry point, so
//!   accounts running this code as global code can receive sponsor relays.

use near_sdk::json_types::Base64VecU8;
use near_sdk::{assert_one_yocto, env, near, serde_json, AccountId, Promise, PublicKey};

#[near(contract_state)]
#[derive(Default)]
pub struct Registrar;

#[near]
impl Registrar {
    #[payable]
    pub fn create_account(
        &mut self,
        new_account_id: AccountId,
        new_public_key: PublicKey,
    ) -> Promise {
        Promise::new(new_account_id)
            .create_account()
            .add_full_access_key(new_public_key)
            .transfer(env::attached_deposit())
    }

    #[private]
    pub fn deploy_global_by_account_id(&mut self, code: Base64VecU8) -> Promise {
        Promise::new(env::current_account_id()).deploy_global_contract_by_account_id(code.0)
    }

    /// Wallet-contract stub: requires 1 yoctoNEAR like the real one and logs
    /// what it received.
    #[payable]
    pub fn w_execute_signed(&mut self, msg: serde_json::Value, proof: String) {
        assert_one_yocto();
        env::log_str(&format!("w_execute_signed proof={proof} msg={msg}"));
    }
}
