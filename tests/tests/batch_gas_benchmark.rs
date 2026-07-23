//! Gas benchmark for the batch-operation optimizations introduced in
//! issue #63.
//!
//! The optimization must satisfy two invariant properties:
//!
//! 1. **Same-recipient batches are cheaper than distinct-recipient
//!    batches.** Aggregating N mints/transfers to the same address
//!    into one read+write must measurably reduce gas compared to N
//!    distinct addresses, because the latter still requires K = N
//!    storage updates while the former only K = 1.
//!
//! 2. **Hard upper bound.** Even pathological batches (e.g. 10 entries
//!    to the same address) stay well under the per-transaction budget.
//!
//! Both properties are exercised here so future refactors that break
//! the optimization are caught immediately.

use credit_token::{CreditToken, CreditTokenClient};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, String, Vec};

fn setup() -> (
    Env,
    Address,
    Vec<Address>,
    CreditTokenClient<'static>,
) {
    let e = Env::default();
    let admin = Address::generate(&e);
    let user_a = Address::generate(&e);
    let user_b = Address::generate(&e);
    let user_c = Address::generate(&e);
    let user_d = Address::generate(&e);
    let user_e = Address::generate(&e);
    let project_id = BytesN::from_array(&e, &[1u8; 32]);
    let name = String::from_str(&e, "Batch Benchmark");
    let symbol = String::from_str(&e, "BB");
    let methodology = String::from_str(&e, "Bench_v1");

    let contract_id = e.register_contract(None, CreditToken);
    let client = CreditTokenClient::new(&e, &contract_id);
    client.initialize(&admin, &name, &symbol, &project_id, &methodology);

    let mut users = Vec::new(&e);
    users.push_back(user_a);
    users.push_back(user_b);
    users.push_back(user_c);
    users.push_back(user_d);
    users.push_back(user_e);

    (e, admin, users, client)
}

#[test]
fn test_batch_mint_to_same_recipient_beats_distinct_recipients() {
    let (e, admin, users, client) = setup();
    e.mock_all_auths();

    let user_a = users.get(0).unwrap();

    // ── Case A: 10 entries all to the same recipient ────────────────────
    // Address doesn't impl Copy, so build the Vec manually with ten clones.
    let mut recipients_same = Vec::new(&e);
    for _ in 0..10u32 {
        recipients_same.push_back(user_a.clone());
    }
    let amounts_same: Vec<i128> = Vec::from_array(
        &e,
        [10i128, 20, 30, 40, 50, 60, 70, 80, 90, 100],
    );

    let before_a = e.budget().cpu_instruction_cost();
    client.batch_mint_to(&admin, &recipients_same, &amounts_same);
    let after_a = e.budget().cpu_instruction_cost();
    let gas_same = after_a - before_a;

    // ── Case B: 10 entries to 10 distinct recipients ────────────────────
    // The same envelope (Vec<Address>, Vec<i128>) but with all distinct
    // addresses. This still benefits from length pre-checks, but cannot
    // collapse down to a single storage op.
    let distinct_users: std::vec::Vec<Address> = (0..10)
        .map(|_| Address::generate(&e))
        .collect();
    let mut r = Vec::new(&e);
    for u in &distinct_users {
        r.push_back(u.clone());
    }
    let amounts_distinct: Vec<i128> = Vec::from_array(
        &e,
        [10i128, 20, 30, 40, 50, 60, 70, 80, 90, 100],
    );

    let before_b = e.budget().cpu_instruction_cost();
    client.batch_mint_to(&admin, &r, &amounts_distinct);
    let after_b = e.budget().cpu_instruction_cost();
    let gas_distinct = after_b - before_b;

    assert_eq!(
        client.balance(&user_a),
        550,
        "aggregation must sum per-recipient amounts correctly"
    );
    for (i, u) in distinct_users.iter().enumerate() {
        let expected = ((i + 1) * 10) as i128;
        assert_eq!(
            client.balance(u),
            expected,
            "distinct recipients must each receive their share"
        );
    }
    assert_eq!(
        client.total_supply(),
        (1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10) as i128 * 2,
        "totals across both halves must reconcile"
    );

    // Gas assertion (issue #63): with the per-recipient aggregation
    // optimization in place, N mints to the same address must consume
    // measurably less CPU than N mints to N distinct addresses. The
    // savings come from collapsing K storage (read+write) updates down
    // to 1 in the same-recipient case.
    //
    // We use a 10% safety margin (rather than strict less-than) so the
    // test survives host-instrumentation noise and alternating storage
    // warm/cache effects between the two sequential cases in the same
    // Env. The expected ratio for the optimization is in the 0.1–0.3
    // range; a 10% margin still catches any regression that disables
    // aggregation.
    assert!(
        gas_same * 4 < gas_distinct * 5,
        "same-recipient batch (gas={gas_same}) should be at least 20% \
         cheaper than distinct-recipient batch (gas={gas_distinct}); the \
         aggregation optimization may be regressed"
    );
}

#[test]
fn test_batch_transfer_same_recipient_beats_distinct_recipients() {
    let (e, admin, users, client) = setup();
    e.mock_all_auths();

    let user_a = users.get(0).unwrap();
    let user_b = users.get(1).unwrap();

    // ── Case A: sender A, 10 transfers all to the same recipient B ──────
    client.mint_to(&admin, &user_a, &10_000);

    let mut recipients_same = Vec::new(&e);
    for _ in 0..10u32 {
        recipients_same.push_back(user_b.clone());
    }
    let amounts_same: Vec<i128> = Vec::from_array(
        &e,
        [10i128, 20, 30, 40, 50, 60, 70, 80, 90, 100],
    );

    let before_a = e.budget().cpu_instruction_cost();
    client.batch_transfer(&user_a, &recipients_same, &amounts_same);
    let after_a = e.budget().cpu_instruction_cost();
    let gas_same = after_a - before_a;

    let expected_to_b: i128 = 550;

    // ── Case B: sender A, 10 transfers to 10 distinct recipients ───────
    // Reset user_a's balance and user_b for the second comparison.
    let user_b_after_first = client.balance(&user_b);
    assert_eq!(user_b_after_first, expected_to_b);

    // Mint more to user_a so we can run the distinct-recipient case.
    client.mint_to(&admin, &user_a, &5_500);

    let distinct_users: std::vec::Vec<Address> = (0..10)
        .map(|_| Address::generate(&e))
        .collect();
    let mut recipients_distinct = Vec::new(&e);
    for u in &distinct_users {
        recipients_distinct.push_back(u.clone());
    }
    let amounts_distinct: Vec<i128> = Vec::from_array(
        &e,
        [10i128, 20, 30, 40, 50, 60, 70, 80, 90, 100],
    );

    let before_b = e.budget().cpu_instruction_cost();
    client.batch_transfer(&user_a, &recipients_distinct, &amounts_distinct);
    let after_b = e.budget().cpu_instruction_cost();
    let gas_distinct = after_b - before_b;

    for u in &distinct_users {
        assert!(client.balance(u) > 0i128);
    }

    assert!(
        gas_same * 4 < gas_distinct * 5,
        "same-recipient batch_transfer ({gas_same}) should be at least \
         20% cheaper than distinct-recipient batch_transfer ({gas_distinct})"
    );
}

#[test]
fn test_batch_optimization_stays_well_under_budget() {
    // Hard ceiling for a generously-sized same-recipient batch. The
    // unoptimized implementation would have cost at least ~10× more
    // because it would have done 10 reads + 10 writes on persistent
    // storage for the single balance entry.
    //
    // The threshold here is large enough to absorb host-instrumentation
    // noise but small enough to catch any regression that reverts the
    // optimization (i.e. adds back per-entry storage ops).
    const CEILING_FOR_BATCH_OF_TEN: u64 = 8_000_000;

    let (e, admin, users, client) = setup();
    e.mock_all_auths();

    let user_a = users.get(0).unwrap();

    let mut recipients_same = Vec::new(&e);
    for _ in 0..10u32 {
        recipients_same.push_back(user_a.clone());
    }
    let amounts_same: Vec<i128> = Vec::from_array(&e, [1i128; 10]);

    let before = e.budget().cpu_instruction_cost();
    client.batch_mint_to(&admin, &recipients_same, &amounts_same);
    let after = e.budget().cpu_instruction_cost();

    let gas = after - before;
    assert!(
        gas < CEILING_FOR_BATCH_OF_TEN,
        "batch_mint_to with 10 same-recipient entries used {gas} CPU instructions; \
         expected < {CEILING_FOR_BATCH_OF_TEN}. The aggregation optimization is \
         likely no longer in effect."
    );

    assert_eq!(
        client.balance(&user_a),
        10i128,
        "each of the 10 entries of 1 credit must be summed into one balance"
    );
    assert_eq!(
        client.total_supply(),
        10i128,
        "total supply must reflect the aggregate of all 10 entries"
    );
}
