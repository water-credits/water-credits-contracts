//! Integration tests for Issue #36: the verification oracle must never let a
//! `max_supply` cap breach roll back window finalization.
//!
//! The oracle reads `total_supply()`/`max_supply()` from the credit token and
//! mints at most the remaining allowance, recording the actual amount in
//! `VerificationResult.credits_minted`. These tests exercise the real
//! `credit_token` contract end-to-end (not a mock) to prove the cap is honored.

use credit_token::{CreditToken, CreditTokenClient};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, String, Vec};
use verification_oracle::{VerificationOracle, VerificationOracleClient};

/// Readings that produce exactly 50 total_credits:
/// ph=700 turb=10 do=80 flow=500 temp=250 n=10 p=2
///   - n removal: med_n(10) == baseline(10) → 0
///   - p removal: med_p(2)  == baseline(2)  → 0
///   - volumetric: flow 500 > 0 → 500*100/1000 = 50
///   - penalty: 0 (all medians within thresholds)
///   - total = 50 * (10000 - 0) / 10000 = 50
const READING: (i64, i64, i64, i64, i64, i64, i64) = (700, 10, 80, 500, 250, 10, 2);

struct Fixture {
    _e: Env,
    admin: Address,
    oracle_client: VerificationOracleClient<'static>,
    token_client: CreditTokenClient<'static>,
    beneficiary: Address,
    project_id: BytesN<32>,
    oracles: Vec<Address>,
}

fn setup() -> Fixture {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let beneficiary = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[7u8; 32]);

    let oracle_id = e.register_contract(None, VerificationOracle);
    let oracle_client = VerificationOracleClient::new(&e, &oracle_id);
    let staking_token = Address::generate(&e);
    let treasury = Address::generate(&e);
    oracle_client.initialize(&admin, &staking_token, &treasury);

    let token_id = e.register_contract(None, CreditToken);
    let token_client = CreditTokenClient::new(&e, &token_id);
    let token_project_id = BytesN::from_array(&e, &[1u8; 32]);
    token_client.initialize(
        &admin,
        &String::from_str(&e, "Test Credits"),
        &String::from_str(&e, "TST"),
        &token_project_id,
        &String::from_str(&e, "Wetland_Restoration_v1.0"),
    );
    // The oracle invokes mint_to as itself; it must be the designated minter.
    token_client.set_minter(&admin, &oracle_id);

    // Wire the project to the token + beneficiary.
    oracle_client.set_project_config(&admin, &project_id, &token_id, &beneficiary);

    // Disable the min-stake requirement so oracles can be added without funding.
    let mut config = oracle_client.get_config();
    config.min_stake = 0;
    oracle_client.update_config(&admin, &config);

    let mut oracles = Vec::new(&e);
    for _ in 0..3u32 {
        let o = Address::generate(&e);
        oracle_client.add_oracle(&admin, &o);
        oracles.push_back(o);
    }

    Fixture {
        _e: e,
        admin,
        oracle_client,
        token_client,
        beneficiary,
        project_id,
        oracles,
    }
}

fn submit_three(f: &Fixture) {
    let (ph, turb, do_, flow, temp, n, p) = READING;
    f.oracle_client.submit_reading(
        &f.oracles.get(0).unwrap(),
        &f.project_id,
        &1,
        &ph,
        &turb,
        &do_,
        &flow,
        &temp,
        &n,
        &p,
    );
    f.oracle_client.submit_reading(
        &f.oracles.get(1).unwrap(),
        &f.project_id,
        &1,
        &ph,
        &turb,
        &do_,
        &flow,
        &temp,
        &n,
        &p,
    );
    f.oracle_client.submit_reading(
        &f.oracles.get(2).unwrap(),
        &f.project_id,
        &1,
        &ph,
        &turb,
        &do_,
        &flow,
        &temp,
        &n,
        &p,
    );
}

#[test]
fn test_mint_respects_max_supply_cap_partial() {
    let f = setup();

    // max_supply = 100, already 90 minted → only 10 remain.
    f.token_client.set_max_supply(&f.admin, &100);
    f.token_client
        .mint_to(&f.oracle_client.address, &f.beneficiary, &90);
    assert_eq!(f.token_client.total_supply(), 90);

    submit_three(&f);

    let res = f.oracle_client.get_last_result(&f.project_id).unwrap();
    assert_eq!(res.total_credits, 50);
    // Only the remaining 10 of the cap could be minted.
    assert_eq!(res.credits_minted, 10);
    assert_eq!(f.token_client.total_supply(), 100);
    assert_eq!(f.token_client.max_supply(), 100);
}

#[test]
fn test_mint_at_exact_max_supply_no_panic() {
    let f = setup();

    // max_supply = 100, already 100 minted → nothing remains.
    f.token_client.set_max_supply(&f.admin, &100);
    f.token_client
        .mint_to(&f.oracle_client.address, &f.beneficiary, &100);
    assert_eq!(f.token_client.total_supply(), 100);

    submit_three(&f);

    // Window finalizes cleanly despite the cap being exhausted.
    let res = f.oracle_client.get_last_result(&f.project_id).unwrap();
    assert_eq!(res.total_credits, 50);
    assert_eq!(res.credits_minted, 0);
    assert_eq!(f.token_client.total_supply(), 100);
}

#[test]
fn test_mint_uncapped_when_max_supply_zero() {
    let f = setup();

    // max_supply defaults to 0 → uncapped.
    assert_eq!(f.token_client.max_supply(), 0);

    submit_three(&f);

    let res = f.oracle_client.get_last_result(&f.project_id).unwrap();
    assert_eq!(res.total_credits, 50);
    assert_eq!(res.credits_minted, 50);
    assert_eq!(f.token_client.total_supply(), 50);
}
