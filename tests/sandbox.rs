//! End-to-end tests on a local neard 2.14 sandbox (protocol 88): the sponsor
//! is called the way connectors will call it, through a public
//! function-call access key restricted to the `create_*` methods.

use near_sdk::state_init::{StateInit, StateInitV1};
use near_sdk::universal_state_init::{UniversalStateInit, UniversalStateInitV1};
use near_sdk::GlobalContractId;
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{AccessKey, Gas, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, AccountId, ContractState, Worker};
use serde_json::json;

const PUBLIC_METHODS: [&str; 5] = [
    "create_named_account",
    "create_implicit_account",
    "create_deterministic_account",
    "create_universal_account",
    "relay_execute_signed",
];

/// Both contracts, compiled once per test run: concurrent `cargo near`
/// builds of the same crate race on the output file.
async fn wasms() -> &'static (Vec<u8>, Vec<u8>) {
    static WASMS: tokio::sync::OnceCell<(Vec<u8>, Vec<u8>)> = tokio::sync::OnceCell::const_new();
    WASMS
        .get_or_init(|| async {
            let sponsor = near_workspaces::compile_project("./")
                .await
                .expect("build sponsor");
            let registrar = near_workspaces::compile_project("./registrar-mock")
                .await
                .expect("build registrar-mock");
            (sponsor, registrar)
        })
        .await
}

struct Env {
    worker: Worker<Sandbox>,
    /// The sponsor account (`helper.trezu.near` in production), holding the contract.
    sponsor: Account,
    /// The same account, but signing with the public function-call key.
    public: Account,
    /// Registrar stand-in, also the account id of the allowlisted global code.
    registrar: Account,
}

async fn setup(max_accounts_per_window: u32) -> anyhow::Result<Env> {
    let worker = near_workspaces::sandbox().await?;
    let (sponsor_wasm, registrar_wasm) = wasms().await;
    let root = worker.root_account()?;

    let registrar = root
        .create_subaccount("registrar")
        .initial_balance(NearToken::from_near(100))
        .transact()
        .await?
        .into_result()?;
    registrar.deploy(registrar_wasm).await?.into_result()?;
    // Publish the mock's code as a global contract identified by this account.
    registrar
        .call(registrar.id(), "deploy_global_by_account_id")
        .args_json(json!({ "code": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, registrar_wasm) }))
        .max_gas()
        .transact()
        .await?
        .into_result()?;

    let sponsor = root
        .create_subaccount("sponsor")
        .initial_balance(NearToken::from_near(20))
        .transact()
        .await?
        .into_result()?;
    sponsor.deploy(sponsor_wasm).await?.into_result()?;
    sponsor
        .call(sponsor.id(), "new")
        .args_json(json!({
            "owner_id": root.id(),
            "registrar_id": registrar.id(),
            "allowed_codes": [{ "account_id": registrar.id() }],
            "window_secs": "3600",
            "max_accounts_per_window": max_accounts_per_window,
            "max_relays_per_window": 10,
        }))
        .transact()
        .await?
        .into_result()?;

    let public_sk = SecretKey::from_random(KeyType::ED25519);
    sponsor
        .batch(sponsor.id())
        .add_key(
            public_sk.public_key(),
            AccessKey::function_call_access(
                sponsor.id(),
                &PUBLIC_METHODS,
                Some(NearToken::from_near(1)),
            ),
        )
        .transact()
        .await?
        .into_result()?;
    let public = Account::from_secret_key(sponsor.id().clone(), public_sk, &worker);

    Ok(Env {
        worker,
        sponsor,
        public,
        registrar,
    })
}

impl Env {
    async fn call(
        &self,
        method: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<ExecutionFinalResult> {
        Ok(self
            .public
            .call(self.sponsor.id(), method)
            .args_json(args)
            .gas(Gas::from_tgas(150))
            .transact()
            .await?)
    }

    async fn exists(&self, account_id: &AccountId) -> bool {
        self.worker.view_account(account_id).await.is_ok()
    }

    async fn sponsor_balance(&self) -> anyhow::Result<NearToken> {
        Ok(self.worker.view_account(self.sponsor.id()).await?.balance)
    }

    fn code(&self) -> GlobalContractId {
        GlobalContractId::AccountId(self.registrar.id().as_str().parse().unwrap())
    }
}

fn random_ed25519() -> (String, String) {
    let pk = SecretKey::from_random(KeyType::ED25519).public_key();
    let pk_str = pk.to_string();
    let bytes = bs58::decode(pk_str.trim_start_matches("ed25519:"))
        .into_vec()
        .unwrap();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    (pk_str, hex)
}

fn assert_failure(result: &ExecutionFinalResult, needle: &str) {
    assert!(
        result.is_failure(),
        "expected failure containing {needle:?}"
    );
    let text = format!("{:?}", result.clone().into_result().unwrap_err());
    assert!(text.contains(needle), "expected {needle:?} in {text}");
}

#[tokio::test]
async fn creates_all_four_kinds_through_the_public_key() -> anyhow::Result<()> {
    let env = setup(10).await?;
    let balance_before = env.sponsor_balance().await?;

    // Named: <name>.<registrar> via registrar.create_account.
    let (pk, _) = random_ed25519();
    let named: AccountId = format!("alice.{}", env.registrar.id()).parse()?;
    let result = env
        .call(
            "create_named_account",
            json!({ "account_id": named, "public_key": pk }),
        )
        .await?;
    assert!(result.is_success(), "{:?}", result.into_result());
    let keys = env.worker.view_access_keys(&named).await?;
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].public_key.to_string(), pk);

    // Implicit: 0 NEAR transfer to the key's hex id.
    let (_, implicit_hex) = random_ed25519();
    let implicit: AccountId = implicit_hex.parse()?;
    assert!(!env.exists(&implicit).await);
    let result = env
        .call("create_implicit_account", json!({ "account_id": implicit }))
        .await?;
    assert!(result.is_success(), "{:?}", result.into_result());
    assert!(env.exists(&implicit).await);

    // Deterministic (NEP-616): 0s… account running the allowlisted global code.
    let state_init =
        StateInit::V1(StateInitV1::code(env.code()).with_data_entry(b"k".to_vec(), b"v".to_vec()));
    let deterministic: AccountId = state_init.derive_account_id().as_str().parse()?;
    let result = env
        .call(
            "create_deterministic_account",
            json!({ "state_init": state_init }),
        )
        .await?;
    assert!(result.is_success(), "{:?}", result.into_result());
    let view = env.worker.view_account(&deterministic).await?;
    assert_eq!(
        view.contract_state,
        ContractState::GlobalAccountId(env.registrar.id().clone())
    );

    // Universal (NEP-655), key-only: 0u… account controlled by an access key.
    let (pk, _) = random_ed25519();
    let raw = UniversalStateInit::V1(
        UniversalStateInitV1::default().with_access_key(pk.parse::<near_sdk::PublicKey>().unwrap()),
    )
    .to_raw();
    let universal: AccountId = raw.try_derive_account_id().unwrap().as_str().parse()?;
    let result = env
        .call("create_universal_account", json!({ "state_init": raw }))
        .await?;
    assert!(result.is_success(), "{:?}", result.into_result());
    let keys = env.worker.view_access_keys(&universal).await?;
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].public_key.to_string(), pk);

    // Universal with the allowlisted code.
    let raw =
        UniversalStateInit::V1(UniversalStateInitV1::default().with_code(env.code())).to_raw();
    let universal_code: AccountId = raw.try_derive_account_id().unwrap().as_str().parse()?;
    let result = env
        .call("create_universal_account", json!({ "state_init": raw }))
        .await?;
    assert!(result.is_success(), "{:?}", result.into_result());
    let view = env.worker.view_account(&universal_code).await?;
    assert_eq!(
        view.contract_state,
        ContractState::GlobalAccountId(env.registrar.id().clone())
    );

    // The sponsor's balance paid only gas for the five calls (a function-call
    // key's allowance caps that spend; it doesn't pay it), no deposits.
    let spent = balance_before.as_yoctonear() - env.sponsor_balance().await?.as_yoctonear();
    assert!(
        spent < NearToken::from_millinear(50).as_yoctonear(),
        "spent {spent} yoctoNEAR"
    );
    Ok(())
}

#[tokio::test]
async fn refuses_what_the_public_key_must_not_do() -> anyhow::Result<()> {
    let env = setup(10).await?;

    let (pk, _) = random_ed25519();
    let result = env
        .call(
            "create_named_account",
            json!({ "account_id": "squat.near", "public_key": pk }),
        )
        .await?;
    assert_failure(&result, "Only direct sub-accounts of");

    let other = StateInit::V1(StateInitV1::code(GlobalContractId::AccountId(
        "evil.test.near".parse().unwrap(),
    )));
    let result = env
        .call(
            "create_deterministic_account",
            json!({ "state_init": other }),
        )
        .await?;
    assert_failure(&result, "not on the sponsor's allowlist");

    // Admin methods are not on the key's method list…
    let result = env
        .public
        .call(env.sponsor.id(), "set_rate_limits")
        .args_json(json!({ "window_secs": "1", "max_accounts_per_window": 1_000_000, "max_relays_per_window": 1_000_000 }))
        .transact()
        .await;
    assert!(result.is_err() || result.unwrap().is_failure());

    // …and even an unrestricted function-call key can't attach the 1 yoctoNEAR
    // they require.
    let any_method_sk = SecretKey::from_random(KeyType::ED25519);
    env.sponsor
        .batch(env.sponsor.id())
        .add_key(
            any_method_sk.public_key(),
            AccessKey::function_call_access(env.sponsor.id(), &[], Some(NearToken::from_near(1))),
        )
        .transact()
        .await?
        .into_result()?;
    let any_method = Account::from_secret_key(env.sponsor.id().clone(), any_method_sk, &env.worker);
    let result = any_method
        .call(env.sponsor.id(), "set_rate_limits")
        .args_json(json!({ "window_secs": "1", "max_accounts_per_window": 1_000_000, "max_relays_per_window": 1_000_000 }))
        .transact()
        .await?;
    assert_failure(&result, "Requires attached deposit of exactly 1 yoctoNEAR");
    let result = any_method
        .call(env.sponsor.id(), "set_rate_limits")
        .args_json(json!({ "window_secs": "1", "max_accounts_per_window": 1_000_000, "max_relays_per_window": 1_000_000 }))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await;
    assert!(result.is_err() || result.unwrap().is_failure());

    let config: serde_json::Value = env
        .sponsor
        .view(env.sponsor.id(), "get_config")
        .await?
        .json()?;
    assert_eq!(config["max_accounts_per_window"], 10);
    Ok(())
}

#[tokio::test]
async fn rate_limit_caps_creations() -> anyhow::Result<()> {
    let env = setup(2).await?;
    for _ in 0..2 {
        let (_, hex) = random_ed25519();
        let result = env
            .call("create_implicit_account", json!({ "account_id": hex }))
            .await?;
        assert!(result.is_success(), "{:?}", result.into_result());
    }
    let (_, hex) = random_ed25519();
    let result = env
        .call("create_implicit_account", json!({ "account_id": hex }))
        .await?;
    assert_failure(&result, "Too many accounts created recently");

    // The owner (full-access key holder elsewhere) can raise the limit.
    let owner = env.worker.root_account()?;
    owner
        .call(env.sponsor.id(), "set_rate_limits")
        .args_json(json!({ "window_secs": "3600", "max_accounts_per_window": 3, "max_relays_per_window": 10 }))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await?
        .into_result()?;
    let result = env
        .call("create_implicit_account", json!({ "account_id": hex }))
        .await?;
    assert!(result.is_success(), "{:?}", result.into_result());
    Ok(())
}

fn relay_args(proof: &str) -> String {
    let args = json!({ "msg": { "signer_id": "wallet", "nonce": 7 }, "proof": proof }).to_string();
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, args)
}

#[tokio::test]
async fn relays_w_execute_signed_to_wallet_accounts() -> anyhow::Result<()> {
    let env = setup(10).await?;

    // Deterministic wallet that doesn't exist yet: created by the relay.
    let state_init = StateInit::V1(
        StateInitV1::code(env.code()).with_data_entry(b"pk".to_vec(), b"seed".to_vec()),
    );
    let wallet: AccountId = state_init.derive_account_id().as_str().parse()?;
    assert!(!env.exists(&wallet).await);
    let relay = |proof: &'static str| {
        env.call(
            "relay_execute_signed",
            json!({ "state_init": { "deterministic": state_init }, "args": relay_args(proof) }),
        )
    };
    let result = relay("p1").await?;
    assert!(result.is_success(), "{:?}", result.clone().into_result());
    assert!(
        result
            .logs()
            .iter()
            .any(|l| l.contains(r#"w_execute_signed proof=p1 prev_pk=Some("seed")"#)),
        "{:?}",
        result.logs()
    );
    let view = env.worker.view_account(&wallet).await?;
    assert_eq!(
        view.contract_state,
        ContractState::GlobalAccountId(env.registrar.id().clone())
    );

    // Same wallet, now existing: the repeated state init is a no-op and leaves
    // the wallet's storage alone (it sees what the first call wrote).
    let result = relay("p2").await?;
    assert!(result.is_success(), "{:?}", result.clone().into_result());
    assert!(
        result
            .logs()
            .iter()
            .any(|l| l.contains(r#"w_execute_signed proof=p2 prev_pk=Some("p1")"#)),
        "{:?}",
        result.logs()
    );

    // Universal wallet with code.
    let raw =
        UniversalStateInit::V1(UniversalStateInitV1::default().with_code(env.code())).to_raw();
    let result = env
        .call(
            "relay_execute_signed",
            json!({ "state_init": { "universal": raw }, "args": relay_args("p3") }),
        )
        .await?;
    assert!(result.is_success(), "{:?}", result.clone().into_result());
    assert!(result
        .logs()
        .iter()
        .any(|l| l.contains("w_execute_signed proof=p3")));

    // A wallet running code that isn't on the allowlist is refused.
    let other = StateInit::V1(StateInitV1::code(GlobalContractId::AccountId(
        "evil.test.near".parse().unwrap(),
    )));
    let result = env
        .call(
            "relay_execute_signed",
            json!({ "state_init": { "deterministic": other }, "args": relay_args("p4") }),
        )
        .await?;
    assert_failure(&result, "not on the sponsor's allowlist");

    let config: serde_json::Value = env
        .sponsor
        .view(env.sponsor.id(), "get_config")
        .await?
        .json()?;
    assert_eq!(config["remaining_relays_in_window"], 7);
    assert_eq!(config["remaining_accounts_in_window"], 10);
    Ok(())
}

#[tokio::test]
async fn other_accounts_cannot_use_the_sponsor() -> anyhow::Result<()> {
    let env = setup(10).await?;
    let outsider = env.worker.dev_create_account().await?;
    let (_, hex) = random_ed25519();
    let result = outsider
        .call(env.sponsor.id(), "create_implicit_account")
        .args_json(json!({ "account_id": hex }))
        .transact()
        .await?;
    assert_failure(
        &result,
        "Only callable with the sponsor account's own access keys",
    );
    Ok(())
}
