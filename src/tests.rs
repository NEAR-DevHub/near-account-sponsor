use super::*;
use near_sdk::state_init::StateInitV1;
use near_sdk::test_utils::{get_logs, VMContextBuilder};
use near_sdk::testing_env;
use near_sdk::universal_state_init::UniversalStateInitV1;

const SPONSOR: &str = "helper.trezu.near";
const OWNER: &str = "trezu.near";
const ED25519_KEY: &str = "ed25519:4fJLrgA11h1136q8ERGbQGfB7dQr7AC3vSDMy115nU5q";

fn wallet_code() -> GlobalContractId {
    GlobalContractId::AccountId("eip712-wallet-contract.trezu.near".parse().unwrap())
}

fn other_code() -> GlobalContractId {
    GlobalContractId::AccountId("someone-else.near".parse().unwrap())
}

/// Context of a call made through the sponsor's own (function-call) key.
fn context(block_timestamp_ns: u64) -> VMContextBuilder {
    let mut ctx = VMContextBuilder::new();
    ctx.current_account_id(SPONSOR.parse().unwrap())
        .predecessor_account_id(SPONSOR.parse().unwrap())
        .signer_account_id(SPONSOR.parse().unwrap())
        .block_timestamp(block_timestamp_ns);
    ctx
}

fn setup(max_accounts_per_window: u32) -> Contract {
    setup_limits(max_accounts_per_window, 100)
}

fn setup_limits(max_accounts_per_window: u32, max_relays_per_window: u32) -> Contract {
    testing_env!(context(1_000).build());
    Contract::new(
        OWNER.parse().unwrap(),
        "near".parse().unwrap(),
        vec![wallet_code()],
        U64(60),
        max_accounts_per_window,
        max_relays_per_window,
    )
}

fn key() -> PublicKey {
    ED25519_KEY.parse().unwrap()
}

// ─── Named ──────────────────────────────────────────────────────────────────

#[test]
fn named_account_under_registrar() {
    let mut c = setup(10);
    let _ = c.create_named_account("alice.near".parse().unwrap(), key());
    assert_eq!(
        get_logs(),
        vec![
            r#"EVENT_JSON:{"standard":"account_sponsor","version":"1.0.0","event":"account_requested","data":[{"kind":"named","account_id":"alice.near"}]}"#
        ]
    );
}

#[test]
#[should_panic(expected = "Only direct sub-accounts of near can be created")]
fn named_account_must_be_direct_child_of_registrar() {
    let mut c = setup(10);
    let _ = c.create_named_account("team.alice.near".parse().unwrap(), key());
}

#[test]
#[should_panic(expected = "Only direct sub-accounts of near can be created")]
fn named_account_other_top_level_refused() {
    let mut c = setup(10);
    let _ = c.create_named_account("alice.tg".parse().unwrap(), key());
}

// ─── Implicit ───────────────────────────────────────────────────────────────

#[test]
fn implicit_accounts_accepted() {
    let mut c = setup(10);
    let _ = c.create_implicit_account("11".repeat(32).parse().unwrap());
    let _ = c.create_implicit_account(format!("0x{}", "ab".repeat(20)).parse().unwrap());
}

#[test]
#[should_panic(expected = "Expected a NEAR-implicit")]
fn implicit_refuses_named() {
    let mut c = setup(10);
    let _ = c.create_implicit_account("alice.near".parse().unwrap());
}

#[test]
#[should_panic(expected = "Expected a NEAR-implicit")]
fn implicit_refuses_deterministic() {
    let mut c = setup(10);
    let _ = c.create_implicit_account(format!("0s{}", "ab".repeat(20)).parse().unwrap());
}

// ─── Deterministic (NEP-616) ────────────────────────────────────────────────

#[test]
fn deterministic_with_allowed_code() {
    let mut c = setup(10);
    let state_init =
        StateInit::V1(StateInitV1::code(wallet_code()).with_data_entry(vec![], vec![1, 2, 3]));
    let expected = state_init.derive_account_id();
    assert!(expected.as_str().starts_with("0s"));
    let _ = c.create_deterministic_account(state_init);
    assert!(get_logs()[0].contains(&format!(r#""account_id":"{expected}""#)));
}

#[test]
#[should_panic(expected = "not on the sponsor's allowlist")]
fn deterministic_refuses_unknown_code() {
    let mut c = setup(10);
    let _ = c.create_deterministic_account(StateInit::V1(StateInitV1::code(other_code())));
}

// ─── Universal (NEP-655) ────────────────────────────────────────────────────

fn universal(v1: UniversalStateInitV1) -> RawStateInit {
    UniversalStateInit::V1(v1).to_raw()
}

#[test]
fn universal_key_only() {
    let mut c = setup(10);
    let raw = universal(UniversalStateInitV1::default().with_access_key(key()));
    let expected = raw.try_derive_account_id().unwrap();
    assert!(expected.as_str().starts_with("0u"));
    let _ = c.create_universal_account(raw);
    assert!(get_logs()[0].contains(&format!(r#""account_id":"{expected}""#)));
}

#[test]
fn universal_with_allowed_code() {
    let mut c = setup(10);
    let _ = c.create_universal_account(universal(
        UniversalStateInitV1::default().with_code(wallet_code()),
    ));
}

#[test]
#[should_panic(expected = "not on the sponsor's allowlist")]
fn universal_refuses_unknown_code() {
    let mut c = setup(10);
    let _ = c.create_universal_account(universal(
        UniversalStateInitV1::default()
            .with_code(other_code())
            .with_access_key(key()),
    ));
}

#[test]
#[should_panic(expected = "needs contract code or at least one access key")]
fn universal_refuses_uncontrollable_account() {
    let mut c = setup(10);
    let _ = c.create_universal_account(universal(UniversalStateInitV1::default()));
}

#[test]
#[should_panic(expected = "Invalid universal state init")]
fn universal_refuses_trailing_bytes() {
    let mut c = setup(10);
    let mut raw = universal(UniversalStateInitV1::default().with_access_key(key()));
    raw.0.push(0);
    let _ = c.create_universal_account(raw);
}

// ─── Rate limit ─────────────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Too many accounts created recently")]
fn rate_limit_applies_across_kinds() {
    let mut c = setup(2);
    let _ = c.create_implicit_account("11".repeat(32).parse().unwrap());
    let _ = c.create_named_account("alice.near".parse().unwrap(), key());
    let _ = c.create_implicit_account("22".repeat(32).parse().unwrap());
}

#[test]
fn rate_limit_resets_after_window() {
    let mut c = setup(1);
    let _ = c.create_implicit_account("11".repeat(32).parse().unwrap());
    assert_eq!(c.get_config().remaining_accounts_in_window, 0);

    testing_env!(context(1_000 + 60 * 1_000_000_000).build());
    assert_eq!(c.get_config().remaining_accounts_in_window, 1);
    let _ = c.create_implicit_account("22".repeat(32).parse().unwrap());
    assert_eq!(c.get_config().remaining_accounts_in_window, 0);
}

// ─── Admin ──────────────────────────────────────────────────────────────────

fn as_owner(deposit_yocto: u128) {
    let mut ctx = context(1_000);
    ctx.predecessor_account_id(OWNER.parse().unwrap())
        .attached_deposit(NearToken::from_yoctonear(deposit_yocto));
    testing_env!(ctx.build());
}

#[test]
fn owner_manages_allowlist_and_limits() {
    let mut c = setup(10);
    as_owner(1);
    c.add_allowed_code(other_code());
    c.add_allowed_code(other_code());
    c.set_rate_limits(U64(3600), 5, 7);
    let config = c.get_config();
    assert_eq!(config.allowed_codes, vec![wallet_code(), other_code()]);
    assert_eq!(
        (config.max_accounts_per_window, config.window_secs.0),
        (5, 3600)
    );

    c.remove_allowed_code(wallet_code());
    assert_eq!(c.get_config().allowed_codes, vec![other_code()]);
}

#[test]
#[should_panic(expected = "Requires attached deposit of exactly 1 yoctoNEAR")]
fn admin_needs_one_yocto() {
    let mut c = setup(10);
    as_owner(0);
    c.add_allowed_code(other_code());
}

/// The sponsor's own key calls with the sponsor as predecessor, and a
/// function-call key cannot attach a deposit: it must not pass as owner even
/// if the sponsor account were the owner.
#[test]
#[should_panic(expected = "Requires attached deposit of exactly 1 yoctoNEAR")]
fn public_key_cannot_administer() {
    testing_env!(context(1_000).build());
    let mut c = Contract::new(
        SPONSOR.parse().unwrap(),
        "near".parse().unwrap(),
        vec![],
        U64(60),
        10,
        10,
    );
    c.set_rate_limits(U64(1), u32::MAX, u32::MAX);
}

#[test]
#[should_panic(expected = "Only the owner can do this")]
fn non_owner_cannot_administer() {
    let mut c = setup(10);
    let mut ctx = context(1_000);
    ctx.predecessor_account_id("mallory.near".parse().unwrap())
        .attached_deposit(NearToken::from_yoctonear(1));
    testing_env!(ctx.build());
    c.set_owner("mallory.near".parse().unwrap());
}

// ─── Relay ──────────────────────────────────────────────────────────────────

fn wallet_state_init() -> StateInit {
    StateInit::V1(StateInitV1::code(wallet_code()).with_data_entry(vec![], vec![7]))
}

fn relay_args() -> (serde_json::Value, String) {
    (
        serde_json::json!({ "signer_id": "x", "nonce": 1 }),
        "proof".to_string(),
    )
}

#[test]
fn relay_to_existing_deterministic_wallet() {
    let mut c = setup(10);
    let wallet = wallet_state_init().derive_account_id();
    let (msg, proof) = relay_args();
    let _ = c.relay_execute_signed(wallet.clone(), msg, proof, None);
    assert!(get_logs()[0].contains(r#""event":"relay_requested""#));
    assert!(get_logs()[0].contains(&format!(r#""account_id":"{wallet}""#)));
    let config = c.get_config();
    assert_eq!(config.remaining_relays_in_window, 99);
    // Relays don't use up the account-creation budget.
    assert_eq!(config.remaining_accounts_in_window, 10);
}

#[test]
fn relay_with_deterministic_state_init() {
    let mut c = setup(10);
    let state_init = wallet_state_init();
    let wallet = state_init.derive_account_id();
    let (msg, proof) = relay_args();
    let _ = c.relay_execute_signed(
        wallet,
        msg,
        proof,
        Some(WalletStateInit::Deterministic(state_init)),
    );
}

#[test]
fn relay_with_universal_state_init() {
    let mut c = setup(10);
    let raw =
        UniversalStateInit::V1(UniversalStateInitV1::default().with_code(wallet_code())).to_raw();
    let wallet = raw.try_derive_account_id().unwrap();
    assert!(is_wallet_account(&wallet));
    let (msg, proof) = relay_args();
    let _ = c.relay_execute_signed(wallet, msg, proof, Some(WalletStateInit::Universal(raw)));
}

#[test]
#[should_panic(expected = "Relays only go to deterministic")]
fn relay_refuses_named_target() {
    let mut c = setup(10);
    let (msg, proof) = relay_args();
    let _ = c.relay_execute_signed("some-dapp.near".parse().unwrap(), msg, proof, None);
}

#[test]
#[should_panic(expected = "Relays only go to deterministic")]
fn relay_refuses_implicit_target() {
    let mut c = setup(10);
    let (msg, proof) = relay_args();
    let _ = c.relay_execute_signed("11".repeat(32).parse().unwrap(), msg, proof, None);
}

#[test]
#[should_panic(expected = "state_init does not derive to wallet_id")]
fn relay_refuses_mismatched_state_init() {
    let mut c = setup(10);
    let other = StateInit::V1(StateInitV1::code(wallet_code()).with_data_entry(vec![], vec![8]));
    let wallet = other.derive_account_id();
    let (msg, proof) = relay_args();
    let _ = c.relay_execute_signed(
        wallet,
        msg,
        proof,
        Some(WalletStateInit::Deterministic(wallet_state_init())),
    );
}

#[test]
#[should_panic(expected = "not on the sponsor's allowlist")]
fn relay_refuses_unallowlisted_code() {
    let mut c = setup(10);
    let state_init = StateInit::V1(StateInitV1::code(other_code()));
    let wallet = state_init.derive_account_id();
    let (msg, proof) = relay_args();
    let _ = c.relay_execute_signed(
        wallet,
        msg,
        proof,
        Some(WalletStateInit::Deterministic(state_init)),
    );
}

#[test]
#[should_panic(expected = "needs contract code")]
fn relay_refuses_key_only_universal_init() {
    let mut c = setup(10);
    let raw =
        UniversalStateInit::V1(UniversalStateInitV1::default().with_access_key(key())).to_raw();
    let wallet = raw.try_derive_account_id().unwrap();
    let (msg, proof) = relay_args();
    let _ = c.relay_execute_signed(wallet, msg, proof, Some(WalletStateInit::Universal(raw)));
}

#[test]
#[should_panic(expected = "Too many relays recently")]
fn relay_rate_limit() {
    let mut c = setup_limits(10, 1);
    let wallet = wallet_state_init().derive_account_id();
    let (msg, proof) = relay_args();
    let _ = c.relay_execute_signed(wallet.clone(), msg.clone(), proof.clone(), None);
    let _ = c.relay_execute_signed(wallet, msg, proof, None);
}

#[test]
fn wallet_state_init_json_shape() {
    let json = serde_json::json!({ "deterministic": wallet_state_init() });
    let parsed: WalletStateInit = serde_json::from_value(json).unwrap();
    assert!(matches!(parsed, WalletStateInit::Deterministic(_)));
    let parsed: WalletStateInit =
        serde_json::from_value(serde_json::json!({ "universal": "AA==" })).unwrap();
    assert!(matches!(parsed, WalletStateInit::Universal(_)));
}
