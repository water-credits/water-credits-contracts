#![cfg(test)]

use credit_marketplace::{
    calculate_cost, calculate_fee, CreditMarketplace, CreditMarketplaceClient, OrderSide,
    OrderStatus, MAX_OPEN_ORDERS_PER_MAKER, PRICE_PRECISION,
};
use credit_token::{CreditToken, CreditTokenClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    vec, Address, BytesN, Env, String, Vec,
};

fn create_token_contract<'a>(
    e: &Env,
    admin: &Address,
    name: &str,
    symbol: &str,
) -> (Address, CreditTokenClient<'a>) {
    let contract_id = e.register_contract(None, CreditToken);
    let client = CreditTokenClient::new(e, &contract_id);
    let project_id = BytesN::from_array(e, &[1u8; 32]);
    let meth = String::from_str(e, "WTR-01");
    client.initialize(
        admin,
        &String::from_str(e, name),
        &String::from_str(e, symbol),
        &project_id,
        &meth,
    );
    (contract_id, client)
}

fn create_marketplace_contract<'a>(
    e: &Env,
    admin: &Address,
    fee_bps: u32,
    fee_recipient: &Address,
) -> (Address, CreditMarketplaceClient<'a>) {
    let contract_id = e.register_contract(None, CreditMarketplace);
    let client = CreditMarketplaceClient::new(e, &contract_id);
    client.initialize(admin, &fee_bps, fee_recipient);
    (contract_id, client)
}

#[test]
fn test_marketplace_initialize_and_admin_controls() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let new_admin = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (_, client) = create_marketplace_contract(&e, &admin, 250, &fee_recipient);

    assert_eq!(client.admin(), admin);
    let (bps, recipient) = client.fee_info();
    assert_eq!(bps, 250);
    assert_eq!(recipient, fee_recipient);
    assert!(!client.paused());
    assert_eq!(client.order_count(), 0);

    // Update fee
    let new_fee_recipient = Address::generate(&e);
    client.set_fee(&admin, &500, &new_fee_recipient);
    let (bps2, recipient2) = client.fee_info();
    assert_eq!(bps2, 500);
    assert_eq!(recipient2, new_fee_recipient);

    // Pause / unpause
    client.pause(&admin);
    assert!(client.paused());
    client.unpause(&admin);
    assert!(!client.paused());

    // Update admin
    client.set_admin(&admin, &new_admin);
    assert_eq!(client.admin(), new_admin);
}

#[test]
fn test_create_sell_order_and_escrow_safety() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let seller = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (credit_token_id, credit_client) = create_token_contract(&e, &admin, "Water Credit", "WTR");
    let (payment_token_id, _) = create_token_contract(&e, &admin, "USD Coin", "USDC");
    let (market_id, market_client) = create_marketplace_contract(&e, &admin, 100, &fee_recipient);

    // Mint 1,000 credits to seller
    credit_client.mint_to(&admin, &seller, &1_000_0000000);
    assert_eq!(credit_client.balance(&seller), 1_000_0000000);

    // Seller approves marketplace for 600 credits
    credit_client.approve(&seller, &market_id, &600_0000000, &0);

    // Seller lists 600 credits at price 2.5 USDC per credit (25_000_000 in 7 decimals)
    let order_id = market_client.create_sell_order(
        &seller,
        &credit_token_id,
        &payment_token_id,
        &600_0000000,
        &25_000_000,
    );

    assert_eq!(order_id, 1);
    assert_eq!(market_client.order_count(), 1);

    // Verify credits are in escrow at the marketplace address
    assert_eq!(credit_client.balance(&seller), 400_0000000);
    assert_eq!(credit_client.balance(&market_id), 600_0000000);

    // Verify seller cannot double-spend or retire escrowed credits
    // Seller only has 400 credits remaining
    assert_eq!(credit_client.balance(&seller), 400_0000000);

    let order = market_client.get_order(&order_id).unwrap();
    assert_eq!(order.id, 1);
    assert_eq!(order.maker, seller);
    assert_eq!(order.side, OrderSide::Sell);
    assert_eq!(order.amount, 600_0000000);
    assert_eq!(order.filled_amount, 0);
    assert_eq!(order.price_per_credit, 25_000_000);
    assert_eq!(order.status, OrderStatus::Open);

    let open_orders = market_client.get_open_orders_by_maker(&seller);
    assert_eq!(open_orders.len(), 1);
    assert_eq!(open_orders.get(0).unwrap(), 1);
}

#[test]
fn test_fill_sell_order_atomic_settlement_and_fee() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let seller = Address::generate(&e);
    let buyer = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (credit_token_id, credit_client) = create_token_contract(&e, &admin, "Water Credit", "WTR");
    let (payment_token_id, payment_client) = create_token_contract(&e, &admin, "USD Coin", "USDC");
    // 2% protocol fee (200 bps)
    let (market_id, market_client) = create_marketplace_contract(&e, &admin, 200, &fee_recipient);

    // Setup balances
    credit_client.mint_to(&admin, &seller, &100_0000000);
    payment_client.mint_to(&admin, &buyer, &500_0000000);

    // Seller lists 100 credits at price 2.0 USDC per credit (20_000_000)
    credit_client.approve(&seller, &market_id, &100_0000000, &0);
    let order_id = market_client.create_sell_order(
        &seller,
        &credit_token_id,
        &payment_token_id,
        &100_0000000,
        &20_000_000,
    );

    // Buyer approves marketplace for payment
    payment_client.approve(&buyer, &market_id, &500_0000000, &0);

    // Partial fill: Buyer fills 40 credits
    // Cost for 40 credits = (40_0000000 * 20_000_000) / 10_000_000 = 80_0000000 USDC
    // Fee (2%) = (80_0000000 * 200) / 10000 = 1_6000000 USDC
    // Payout to seller = 80 - 1.6 = 78_4000000 USDC
    market_client.fill_order(&buyer, &order_id, &40_0000000);

    assert_eq!(credit_client.balance(&buyer), 40_0000000);
    assert_eq!(credit_client.balance(&market_id), 60_0000000);
    assert_eq!(payment_client.balance(&seller), 78_4000000);
    assert_eq!(payment_client.balance(&fee_recipient), 1_6000000);
    assert_eq!(payment_client.balance(&buyer), 420_0000000);

    let partial_order = market_client.get_order(&order_id).unwrap();
    assert_eq!(partial_order.status, OrderStatus::PartiallyFilled);
    assert_eq!(partial_order.filled_amount, 40_0000000);

    // Fill remaining 60 credits
    // Cost for 60 credits = 120_0000000 USDC
    // Fee = 2.4 USDC = 2_4000000
    // Payout to seller = 117.6 USDC = 117_6000000
    market_client.fill_order(&buyer, &order_id, &60_0000000);

    assert_eq!(credit_client.balance(&buyer), 100_0000000);
    assert_eq!(credit_client.balance(&market_id), 0);
    assert_eq!(payment_client.balance(&seller), 196_0000000); // 78.4 + 117.6 = 196.0 USDC (200 - 4 fee)
    assert_eq!(payment_client.balance(&fee_recipient), 4_0000000); // 1.6 + 2.4 = 4.0 USDC total fee
    assert_eq!(payment_client.balance(&buyer), 300_0000000);

    let filled_order = market_client.get_order(&order_id).unwrap();
    assert_eq!(filled_order.status, OrderStatus::Filled);
    assert_eq!(filled_order.filled_amount, 100_0000000);

    // Maker's open order list is now empty
    let open_orders = market_client.get_open_orders_by_maker(&seller);
    assert_eq!(open_orders.len(), 0);
}

#[test]
fn test_create_buy_order_and_fill() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let buyer = Address::generate(&e);
    let seller = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (credit_token_id, credit_client) = create_token_contract(&e, &admin, "Water Credit", "WTR");
    let (payment_token_id, payment_client) = create_token_contract(&e, &admin, "USD Coin", "USDC");
    // 1% fee (100 bps)
    let (market_id, market_client) = create_marketplace_contract(&e, &admin, 100, &fee_recipient);

    // Setup: Buyer has 1000 USDC, Seller has 200 WTR credits
    payment_client.mint_to(&admin, &buyer, &1000_0000000);
    credit_client.mint_to(&admin, &seller, &200_0000000);

    // Buyer creates buy order (Bid) for 200 credits at price 3.0 USDC per credit (30_000_000)
    // Total cost = 200 * 3.0 = 600 USDC escrowed
    payment_client.approve(&buyer, &market_id, &600_0000000, &0);
    let order_id = market_client.create_buy_order(
        &buyer,
        &credit_token_id,
        &payment_token_id,
        &200_0000000,
        &30_000_000,
    );

    assert_eq!(payment_client.balance(&buyer), 400_0000000);
    assert_eq!(payment_client.balance(&market_id), 600_0000000);

    // Seller fills the buy order with 200 credits
    // Total payment = 600 USDC, Fee (1%) = 6 USDC, Payout to seller = 594 USDC
    credit_client.approve(&seller, &market_id, &200_0000000, &0);
    market_client.fill_order(&seller, &order_id, &200_0000000);

    assert_eq!(credit_client.balance(&seller), 0);
    assert_eq!(credit_client.balance(&buyer), 200_0000000);
    assert_eq!(payment_client.balance(&seller), 594_0000000);
    assert_eq!(payment_client.balance(&fee_recipient), 6_0000000);
    assert_eq!(payment_client.balance(&market_id), 0);

    let order = market_client.get_order(&order_id).unwrap();
    assert_eq!(order.status, OrderStatus::Filled);
}

#[test]
fn test_cancel_sell_order_refunds_escrow() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let seller = Address::generate(&e);
    let buyer = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (credit_token_id, credit_client) = create_token_contract(&e, &admin, "Water Credit", "WTR");
    let (payment_token_id, payment_client) = create_token_contract(&e, &admin, "USD Coin", "USDC");
    let (market_id, market_client) = create_marketplace_contract(&e, &admin, 0, &fee_recipient);

    credit_client.mint_to(&admin, &seller, &500_0000000);
    payment_client.mint_to(&admin, &buyer, &500_0000000);

    // List 500 credits at price 1.0 USDC
    credit_client.approve(&seller, &market_id, &500_0000000, &0);
    let order_id = market_client.create_sell_order(
        &seller,
        &credit_token_id,
        &payment_token_id,
        &500_0000000,
        &10_000_000,
    );

    // Partial fill 100 credits
    payment_client.approve(&buyer, &market_id, &100_0000000, &0);
    market_client.fill_order(&buyer, &order_id, &100_0000000);

    assert_eq!(credit_client.balance(&market_id), 400_0000000);

    // Seller cancels order -> remaining 400 credits refunded
    market_client.cancel_order(&seller, &order_id);

    assert_eq!(credit_client.balance(&seller), 400_0000000);
    assert_eq!(credit_client.balance(&market_id), 0);

    let order = market_client.get_order(&order_id).unwrap();
    assert_eq!(order.status, OrderStatus::Cancelled);
    assert_eq!(market_client.get_open_orders_by_maker(&seller).len(), 0);
}

#[test]
fn test_cancel_buy_order_refunds_payment_escrow() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let buyer = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (credit_token_id, _) = create_token_contract(&e, &admin, "Water Credit", "WTR");
    let (payment_token_id, payment_client) = create_token_contract(&e, &admin, "USD Coin", "USDC");
    let (market_id, market_client) = create_marketplace_contract(&e, &admin, 0, &fee_recipient);

    payment_client.mint_to(&admin, &buyer, &1000_0000000);

    // Buy order for 50 credits at 10.0 USDC = 500 USDC escrowed
    payment_client.approve(&buyer, &market_id, &500_0000000, &0);
    let order_id = market_client.create_buy_order(
        &buyer,
        &credit_token_id,
        &payment_token_id,
        &50_0000000,
        &100_000_000,
    );

    assert_eq!(payment_client.balance(&buyer), 500_0000000);
    assert_eq!(payment_client.balance(&market_id), 500_0000000);

    // Cancel order
    market_client.cancel_order(&buyer, &order_id);

    assert_eq!(payment_client.balance(&buyer), 1000_0000000);
    assert_eq!(payment_client.balance(&market_id), 0);

    let order = market_client.get_order(&order_id).unwrap();
    assert_eq!(order.status, OrderStatus::Cancelled);
}

#[test]
fn test_batch_fill_orders() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let seller1 = Address::generate(&e);
    let seller2 = Address::generate(&e);
    let buyer = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (credit_token_id, credit_client) = create_token_contract(&e, &admin, "Water Credit", "WTR");
    let (payment_token_id, payment_client) = create_token_contract(&e, &admin, "USD Coin", "USDC");
    let (market_id, market_client) = create_marketplace_contract(&e, &admin, 0, &fee_recipient);

    credit_client.mint_to(&admin, &seller1, &100_0000000);
    credit_client.mint_to(&admin, &seller2, &100_0000000);
    payment_client.mint_to(&admin, &buyer, &500_0000000);

    credit_client.approve(&seller1, &market_id, &100_0000000, &0);
    credit_client.approve(&seller2, &market_id, &100_0000000, &0);

    let order_id1 = market_client.create_sell_order(
        &seller1,
        &credit_token_id,
        &payment_token_id,
        &100_0000000,
        &10_000_000,
    );
    let order_id2 = market_client.create_sell_order(
        &seller2,
        &credit_token_id,
        &payment_token_id,
        &100_0000000,
        &10_000_000,
    );

    payment_client.approve(&buyer, &market_id, &200_0000000, &0);

    let order_ids = vec![&e, order_id1, order_id2];
    let fill_amounts = vec![&e, 50_0000000i128, 100_0000000i128];

    market_client.batch_fill_orders(&buyer, &order_ids, &fill_amounts);

    assert_eq!(credit_client.balance(&buyer), 150_0000000);
    assert_eq!(payment_client.balance(&seller1), 50_0000000);
    assert_eq!(payment_client.balance(&seller2), 100_0000000);
}

#[test]
fn test_get_orders_for_token_pagination() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let seller = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (credit1, credit_client1) = create_token_contract(&e, &admin, "Credit 1", "C1");
    let (credit2, credit_client2) = create_token_contract(&e, &admin, "Credit 2", "C2");
    let (payment, _) = create_token_contract(&e, &admin, "USDC", "USDC");
    let (market_id, market_client) = create_marketplace_contract(&e, &admin, 0, &fee_recipient);

    credit_client1.mint_to(&admin, &seller, &100_0000000);
    credit_client2.mint_to(&admin, &seller, &100_0000000);

    credit_client1.approve(&seller, &market_id, &100_0000000, &0);
    credit_client2.approve(&seller, &market_id, &100_0000000, &0);

    market_client.create_sell_order(&seller, &credit1, &payment, &50_0000000, &10_000_000);
    market_client.create_sell_order(&seller, &credit2, &payment, &50_0000000, &10_000_000);
    market_client.create_sell_order(&seller, &credit1, &payment, &50_0000000, &10_000_000);

    let list1 = market_client.get_orders_for_token(&credit1, &1, &10);
    assert_eq!(list1.len(), 2);
    assert_eq!(list1.get(0).unwrap().id, 1);
    assert_eq!(list1.get(1).unwrap().id, 3);

    let list2 = market_client.get_orders_for_token(&credit2, &1, &10);
    assert_eq!(list2.len(), 1);
    assert_eq!(list2.get(0).unwrap().id, 2);
}

#[test]
#[should_panic(expected = "max open orders per maker reached")]
fn test_max_open_orders_per_maker_enforced() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let seller = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (credit, credit_client) = create_token_contract(&e, &admin, "Credit", "CR");
    let (payment, _) = create_token_contract(&e, &admin, "USDC", "USDC");
    let (market_id, market_client) = create_marketplace_contract(&e, &admin, 0, &fee_recipient);

    credit_client.mint_to(&admin, &seller, &1000_0000000);
    credit_client.approve(&seller, &market_id, &1000_0000000, &0);

    for _ in 0..MAX_OPEN_ORDERS_PER_MAKER {
        market_client.create_sell_order(&seller, &credit, &payment, &1_0000000, &10_000_000);
    }

    // Exceeding max orders should panic
    market_client.create_sell_order(&seller, &credit, &payment, &1_0000000, &10_000_000);
}

#[test]
fn test_math_and_cost_calculations() {
    // 1 credit at 1.0 price
    assert_eq!(
        calculate_cost(PRICE_PRECISION, PRICE_PRECISION),
        PRICE_PRECISION
    );
    // 2.5 credits at 4.0 price = 10.0
    assert_eq!(calculate_cost(25_000_000, 40_000_000), 100_000_000);
    // Fee 250 bps (2.5%) of 100_000_000 = 2_500_000
    assert_eq!(calculate_fee(100_000_000, 250), 2_500_000);
    // 0 bps fee
    assert_eq!(calculate_fee(100_000_000, 0), 0);
}

#[test]
#[should_panic(expected = "contract is paused")]
fn test_create_order_when_paused_panics() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let seller = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (credit, credit_client) = create_token_contract(&e, &admin, "Credit", "CR");
    let (payment, _) = create_token_contract(&e, &admin, "USDC", "USDC");
    let (market_id, market_client) = create_marketplace_contract(&e, &admin, 0, &fee_recipient);

    credit_client.mint_to(&admin, &seller, &100_0000000);
    credit_client.approve(&seller, &market_id, &100_0000000, &0);

    market_client.pause(&admin);
    market_client.create_sell_order(&seller, &credit, &payment, &50_0000000, &10_000_000);
}

#[test]
#[should_panic(expected = "contract is paused")]
fn test_fill_order_when_paused_panics() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let seller = Address::generate(&e);
    let buyer = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (credit, credit_client) = create_token_contract(&e, &admin, "Credit", "CR");
    let (payment, payment_client) = create_token_contract(&e, &admin, "USDC", "USDC");
    let (market_id, market_client) = create_marketplace_contract(&e, &admin, 0, &fee_recipient);

    credit_client.mint_to(&admin, &seller, &100_0000000);
    payment_client.mint_to(&admin, &buyer, &100_0000000);

    credit_client.approve(&seller, &market_id, &100_0000000, &0);
    let order_id =
        market_client.create_sell_order(&seller, &credit, &payment, &50_0000000, &10_000_000);

    payment_client.approve(&buyer, &market_id, &100_0000000, &0);
    market_client.pause(&admin);

    market_client.fill_order(&buyer, &order_id, &50_0000000);
}

#[test]
#[should_panic(expected = "unauthorized: caller is not maker")]
fn test_cancel_order_unauthorized_maker_panics() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let seller = Address::generate(&e);
    let attacker = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (credit, credit_client) = create_token_contract(&e, &admin, "Credit", "CR");
    let (payment, _) = create_token_contract(&e, &admin, "USDC", "USDC");
    let (market_id, market_client) = create_marketplace_contract(&e, &admin, 0, &fee_recipient);

    credit_client.mint_to(&admin, &seller, &100_0000000);
    credit_client.approve(&seller, &market_id, &100_0000000, &0);

    let order_id =
        market_client.create_sell_order(&seller, &credit, &payment, &50_0000000, &10_000_000);

    market_client.cancel_order(&attacker, &order_id);
}

#[test]
#[should_panic(expected = "order is not open for filling")]
fn test_cannot_fill_cancelled_order() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let seller = Address::generate(&e);
    let buyer = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (credit, credit_client) = create_token_contract(&e, &admin, "Credit", "CR");
    let (payment, payment_client) = create_token_contract(&e, &admin, "USDC", "USDC");
    let (market_id, market_client) = create_marketplace_contract(&e, &admin, 0, &fee_recipient);

    credit_client.mint_to(&admin, &seller, &100_0000000);
    payment_client.mint_to(&admin, &buyer, &100_0000000);

    credit_client.approve(&seller, &market_id, &100_0000000, &0);
    let order_id =
        market_client.create_sell_order(&seller, &credit, &payment, &50_0000000, &10_000_000);

    market_client.cancel_order(&seller, &order_id);

    payment_client.approve(&buyer, &market_id, &100_0000000, &0);
    market_client.fill_order(&buyer, &order_id, &50_0000000);
}

#[test]
#[should_panic(expected = "fee bps exceeds maximum")]
fn test_fee_bps_exceeding_max_panics() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    create_marketplace_contract(&e, &admin, 1001, &fee_recipient);
}

#[test]
fn test_supply_conservation_invariant_with_marketplace_and_retire() {
    let e = Env::default();
    e.mock_all_auths();

    let admin = Address::generate(&e);
    let seller = Address::generate(&e);
    let buyer = Address::generate(&e);
    let fee_recipient = Address::generate(&e);

    let (credit_token_id, credit_client) = create_token_contract(&e, &admin, "Water Credit", "WTR");
    let (payment_token_id, payment_client) = create_token_contract(&e, &admin, "USD Coin", "USDC");
    let (market_id, market_client) = create_marketplace_contract(&e, &admin, 50, &fee_recipient);

    let initial_mint = 500_0000000i128;
    credit_client.mint_to(&admin, &seller, &initial_mint);
    payment_client.mint_to(&admin, &buyer, &1000_0000000i128);

    // Initial invariant: total_supply == initial_mint, total_retired == 0
    assert_eq!(credit_client.total_supply(), initial_mint);
    assert_eq!(credit_client.total_retired(), 0);

    // Seller lists 300 credits
    credit_client.approve(&seller, &market_id, &300_0000000, &0);
    let order_id = market_client.create_sell_order(
        &seller,
        &credit_token_id,
        &payment_token_id,
        &300_0000000,
        &10_000_000,
    );

    // Invariant maintained: total supply remains 500
    assert_eq!(credit_client.total_supply(), initial_mint);
    assert_eq!(
        credit_client.balance(&seller) + credit_client.balance(&market_id),
        initial_mint
    );

    // Buyer fills 200 credits
    payment_client.approve(&buyer, &market_id, &300_0000000, &0);
    market_client.fill_order(&buyer, &order_id, &200_0000000);

    // Invariant maintained: seller (200) + market (100) + buyer (200) == 500
    assert_eq!(
        credit_client.balance(&seller)
            + credit_client.balance(&market_id)
            + credit_client.balance(&buyer),
        initial_mint
    );

    // Buyer retires 150 of their purchased credits
    credit_client.retire(
        &buyer,
        &150_0000000,
        &String::from_str(&e, "Offsetting water footprint"),
        &String::from_str(&e, "ipfs://cert123"),
    );

    // Invariant: total_supply (350) + total_retired (150) == initial_mint (500)
    assert_eq!(credit_client.total_supply(), 350_0000000);
    assert_eq!(credit_client.total_retired(), 150_0000000);
    assert_eq!(
        credit_client.total_supply() + credit_client.total_retired(),
        initial_mint
    );
    assert_eq!(
        credit_client.balance(&seller)
            + credit_client.balance(&market_id)
            + credit_client.balance(&buyer)
            + credit_client.total_retired(),
        initial_mint
    );
}
