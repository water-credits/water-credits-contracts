#![no_std]
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, vec, Address, Env, IntoVal, Symbol, Vec,
};

#[cfg(test)]
extern crate std;

// ── TTL constants (in ledgers; ~5 sec/ledger on Stellar) ──
/// 1 year ≈ 6 307 200 ledgers.
const MARKET_TTL_THRESHOLD: u32 = 6_307_200;
const MARKET_TTL_BUMP: u32 = 6_307_200;

/// Standard price precision: 10^7 (matching credit_token 7 decimals).
pub const PRICE_PRECISION: i128 = 10_000_000;
/// Maximum open orders permitted per maker account (DoS prevention).
pub const MAX_OPEN_ORDERS_PER_MAKER: u32 = 50;
/// Basis points denominator (100% = 10000).
pub const BPS_DENOMINATOR: i128 = 10_000;
/// Maximum fee bps (10% = 1000 bps).
pub const MAX_FEE_BPS: u32 = 1_000;

// ── Events ──
const EVENT_INITIALIZED: Symbol = symbol_short!("init");
const EVENT_ORDER_CREATED: Symbol = symbol_short!("ord_crt");
const EVENT_ORDER_FILLED: Symbol = symbol_short!("ord_fill");
const EVENT_ORDER_CANCELLED: Symbol = symbol_short!("ord_cnc");
const EVENT_FEE_UPDATED: Symbol = symbol_short!("fee_upd");
const EVENT_PAUSED: Symbol = symbol_short!("paused");
const EVENT_UNPAUSED: Symbol = symbol_short!("unpaused");
const EVENT_ADMIN_CHANGED: Symbol = symbol_short!("adm_chg");

// ── Data Types ──

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OrderSide {
    Buy = 0,
    Sell = 1,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OrderStatus {
    Open = 0,
    Filled = 1,
    PartiallyFilled = 2,
    Cancelled = 3,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Order {
    pub id: u64,
    pub maker: Address,
    pub side: OrderSide,
    pub credit_token: Address,
    pub payment_token: Address,
    pub amount: i128,
    pub filled_amount: i128,
    pub price_per_credit: i128,
    pub created_at: u64,
    pub status: OrderStatus,
}

#[contracttype]
pub enum DataKey {
    Admin,
    FeeBps,
    FeeRecipient,
    OrderCount,
    Paused,
    Order(u64),
    MakerOrders(Address),
}

fn has_admin(e: &Env) -> bool {
    e.storage().instance().has(&DataKey::Admin)
}

fn read_admin(e: &Env) -> Address {
    e.storage().instance().get(&DataKey::Admin).unwrap()
}

fn is_paused(e: &Env) -> bool {
    e.storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
}

fn require_not_paused(e: &Env) {
    if is_paused(e) {
        panic!("contract is paused");
    }
}

fn read_order_count(e: &Env) -> u64 {
    e.storage()
        .instance()
        .get(&DataKey::OrderCount)
        .unwrap_or(0)
}

fn save_order_count(e: &Env, count: u64) {
    e.storage().instance().set(&DataKey::OrderCount, &count);
}

fn read_order(e: &Env, order_id: u64) -> Option<Order> {
    let key = DataKey::Order(order_id);
    let order: Option<Order> = e.storage().persistent().get(&key);
    if order.is_some() {
        e.storage()
            .persistent()
            .extend_ttl(&key, MARKET_TTL_THRESHOLD, MARKET_TTL_BUMP);
    }
    order
}

fn save_order(e: &Env, order: &Order) {
    let key = DataKey::Order(order.id);
    e.storage().persistent().set(&key, order);
    e.storage()
        .persistent()
        .extend_ttl(&key, MARKET_TTL_THRESHOLD, MARKET_TTL_BUMP);
}

fn read_maker_orders(e: &Env, maker: &Address) -> Vec<u64> {
    let key = DataKey::MakerOrders(maker.clone());
    let list: Vec<u64> = e
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(e));
    if !list.is_empty() {
        e.storage()
            .persistent()
            .extend_ttl(&key, MARKET_TTL_THRESHOLD, MARKET_TTL_BUMP);
    }
    list
}

fn save_maker_orders(e: &Env, maker: &Address, orders: &Vec<u64>) {
    let key = DataKey::MakerOrders(maker.clone());
    e.storage().persistent().set(&key, orders);
    e.storage()
        .persistent()
        .extend_ttl(&key, MARKET_TTL_THRESHOLD, MARKET_TTL_BUMP);
}

fn add_maker_order(e: &Env, maker: &Address, order_id: u64) {
    let mut orders = read_maker_orders(e, maker);
    if orders.len() >= MAX_OPEN_ORDERS_PER_MAKER {
        panic!("max open orders per maker reached");
    }
    orders.push_back(order_id);
    save_maker_orders(e, maker, &orders);
}

fn remove_maker_order(e: &Env, maker: &Address, order_id: u64) {
    let orders = read_maker_orders(e, maker);
    let mut updated = Vec::new(e);
    for i in 0..orders.len() {
        let id = orders.get(i).unwrap();
        if id != order_id {
            updated.push_back(id);
        }
    }
    save_maker_orders(e, maker, &updated);
}

fn token_transfer(e: &Env, token: &Address, from: &Address, to: &Address, amount: i128) {
    let transfer_args = vec![e, from.into_val(e), to.into_val(e), amount.into_val(e)];
    e.invoke_contract::<()>(token, &Symbol::new(e, "transfer"), transfer_args);
}

fn token_transfer_from(
    e: &Env,
    token: &Address,
    spender: &Address,
    from: &Address,
    to: &Address,
    amount: i128,
) {
    let transfer_args = vec![
        e,
        spender.into_val(e),
        from.into_val(e),
        to.into_val(e),
        amount.into_val(e),
    ];
    e.invoke_contract::<()>(token, &Symbol::new(e, "transfer_from"), transfer_args);
}

/// Calculate total payment token amount for a given credit amount and price.
/// payment = (amount * price_per_credit) / PRICE_PRECISION
pub fn calculate_cost(amount: i128, price_per_credit: i128) -> i128 {
    if amount <= 0 || price_per_credit <= 0 {
        panic!("amount and price must be positive");
    }
    let numerator = amount
        .checked_mul(price_per_credit)
        .expect("overflow in cost calculation");
    let cost = numerator
        .checked_div(PRICE_PRECISION)
        .expect("division error in cost calculation");
    if cost <= 0 {
        panic!("calculated cost is zero");
    }
    cost
}

/// Calculate protocol fee.
pub fn calculate_fee(cost: i128, fee_bps: u32) -> i128 {
    if fee_bps == 0 {
        return 0;
    }
    let fee_num = cost
        .checked_mul(fee_bps as i128)
        .expect("overflow in fee calculation");
    fee_num
        .checked_div(BPS_DENOMINATOR)
        .expect("division error in fee calculation")
}

#[contract]
pub struct CreditMarketplace;

impl CreditMarketplace {
    fn internal_fill_order(e: &Env, taker: &Address, order_id: u64, fill_amount: i128) {
        if fill_amount <= 0 {
            panic!("fill amount must be positive");
        }

        let mut order = read_order(e, order_id).expect("order not found");

        if order.status != OrderStatus::Open && order.status != OrderStatus::PartiallyFilled {
            panic!("order is not open for filling");
        }

        let remaining = order
            .amount
            .checked_sub(order.filled_amount)
            .expect("underflow in remaining calculation");
        if fill_amount > remaining {
            panic!("fill amount exceeds remaining order amount");
        }

        let cost = calculate_cost(fill_amount, order.price_per_credit);
        let (fee_bps, fee_recipient) = Self::fee_info(e.clone());
        let fee = calculate_fee(cost, fee_bps);
        let payout = cost.checked_sub(fee).expect("underflow in payout");

        let contract_address = e.current_contract_address();

        match order.side {
            OrderSide::Sell => {
                // Taker pays `payment_token` to maker (and fee to fee_recipient)
                // 1. Payout to maker
                if payout > 0 {
                    token_transfer_from(
                        e,
                        &order.payment_token,
                        &contract_address,
                        taker,
                        &order.maker,
                        payout,
                    );
                }
                // 2. Fee to fee recipient
                if fee > 0 {
                    token_transfer_from(
                        e,
                        &order.payment_token,
                        &contract_address,
                        taker,
                        &fee_recipient,
                        fee,
                    );
                }

                // Marketplace delivers escrowed credit_token to taker
                token_transfer(
                    e,
                    &order.credit_token,
                    &contract_address,
                    taker,
                    fill_amount,
                );
            }
            OrderSide::Buy => {
                // Taker sends `credit_token` to maker
                token_transfer_from(
                    e,
                    &order.credit_token,
                    &contract_address,
                    taker,
                    &order.maker,
                    fill_amount,
                );

                // Marketplace delivers escrowed payment_token to taker (payout) & fee_recipient (fee)
                if payout > 0 {
                    token_transfer(e, &order.payment_token, &contract_address, taker, payout);
                }
                if fee > 0 {
                    token_transfer(
                        e,
                        &order.payment_token,
                        &contract_address,
                        &fee_recipient,
                        fee,
                    );
                }
            }
        }

        order.filled_amount = order
            .filled_amount
            .checked_add(fill_amount)
            .expect("overflow in filled amount");

        if order.filled_amount == order.amount {
            order.status = OrderStatus::Filled;
            remove_maker_order(e, &order.maker, order.id);
        } else {
            order.status = OrderStatus::PartiallyFilled;
        }

        save_order(e, &order);

        e.events().publish(
            (EVENT_ORDER_FILLED,),
            (
                order_id,
                taker.clone(),
                fill_amount,
                cost,
                fee,
                order.status,
            ),
        );
    }
}

#[contractimpl]
impl CreditMarketplace {
    /// Initialize the marketplace. Admin only, callable once.
    pub fn initialize(e: Env, admin: Address, fee_bps: u32, fee_recipient: Address) {
        if has_admin(&e) {
            panic!("already initialized");
        }
        if fee_bps > MAX_FEE_BPS {
            panic!("fee bps exceeds maximum");
        }
        e.storage().instance().set(&DataKey::Admin, &admin);
        e.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        e.storage()
            .instance()
            .set(&DataKey::FeeRecipient, &fee_recipient);
        e.storage().instance().set(&DataKey::OrderCount, &0u64);
        e.storage().instance().set(&DataKey::Paused, &false);

        e.events()
            .publish((EVENT_INITIALIZED,), (admin, fee_bps, fee_recipient));
    }

    /// Return current admin.
    pub fn admin(e: Env) -> Address {
        read_admin(&e)
    }

    /// Change admin address.
    pub fn set_admin(e: Env, admin: Address, new_admin: Address) {
        admin.require_auth();
        if admin != read_admin(&e) {
            panic!("unauthorized");
        }
        e.storage().instance().set(&DataKey::Admin, &new_admin);
        e.events()
            .publish((EVENT_ADMIN_CHANGED,), (admin, new_admin));
    }

    /// Set protocol fee in basis points (max 1000 = 10%) and fee recipient.
    pub fn set_fee(e: Env, admin: Address, fee_bps: u32, fee_recipient: Address) {
        admin.require_auth();
        if admin != read_admin(&e) {
            panic!("unauthorized");
        }
        if fee_bps > MAX_FEE_BPS {
            panic!("fee bps exceeds maximum");
        }
        e.storage().instance().set(&DataKey::FeeBps, &fee_bps);
        e.storage()
            .instance()
            .set(&DataKey::FeeRecipient, &fee_recipient);

        e.events()
            .publish((EVENT_FEE_UPDATED,), (fee_bps, fee_recipient));
    }

    /// Return current fee configuration: `(fee_bps, fee_recipient)`.
    pub fn fee_info(e: Env) -> (u32, Address) {
        let fee_bps: u32 = e.storage().instance().get(&DataKey::FeeBps).unwrap_or(0);
        let fee_recipient: Address = e
            .storage()
            .instance()
            .get(&DataKey::FeeRecipient)
            .unwrap_or_else(|| read_admin(&e));
        (fee_bps, fee_recipient)
    }

    /// Pause marketplace operations.
    pub fn pause(e: Env, caller: Address) {
        caller.require_auth();
        if caller != read_admin(&e) {
            panic!("unauthorized");
        }
        e.storage().instance().set(&DataKey::Paused, &true);
        e.events().publish((EVENT_PAUSED,), ());
    }

    /// Unpause marketplace operations.
    pub fn unpause(e: Env, caller: Address) {
        caller.require_auth();
        if caller != read_admin(&e) {
            panic!("unauthorized");
        }
        e.storage().instance().set(&DataKey::Paused, &false);
        e.events().publish((EVENT_UNPAUSED,), ());
    }

    /// Return whether the marketplace is paused.
    pub fn paused(e: Env) -> bool {
        is_paused(&e)
    }

    /// Return total orders ever created.
    pub fn order_count(e: Env) -> u64 {
        read_order_count(&e)
    }

    /// Create a sell order (Ask).
    ///
    /// Escrows `amount` of `credit_token` from `maker` into the marketplace contract.
    /// The maker must have called `credit_token.approve(maker, marketplace, amount, exp)` beforehand.
    pub fn create_sell_order(
        e: Env,
        maker: Address,
        credit_token: Address,
        payment_token: Address,
        amount: i128,
        price_per_credit: i128,
    ) -> u64 {
        require_not_paused(&e);
        maker.require_auth();

        if amount <= 0 {
            panic!("amount must be positive");
        }
        if price_per_credit <= 0 {
            panic!("price must be positive");
        }
        // Verify cost calculation is valid and non-zero
        let _ = calculate_cost(amount, price_per_credit);

        let contract_address = e.current_contract_address();

        // Escrow credit tokens from maker to marketplace
        token_transfer_from(
            &e,
            &credit_token,
            &contract_address,
            &maker,
            &contract_address,
            amount,
        );

        let next_id = read_order_count(&e).checked_add(1).expect("overflow");
        save_order_count(&e, next_id);

        let order = Order {
            id: next_id,
            maker: maker.clone(),
            side: OrderSide::Sell,
            credit_token: credit_token.clone(),
            payment_token: payment_token.clone(),
            amount,
            filled_amount: 0,
            price_per_credit,
            created_at: e.ledger().timestamp(),
            status: OrderStatus::Open,
        };

        save_order(&e, &order);
        add_maker_order(&e, &maker, next_id);

        e.events().publish(
            (EVENT_ORDER_CREATED,),
            (
                next_id,
                maker,
                OrderSide::Sell,
                credit_token,
                payment_token,
                amount,
                price_per_credit,
            ),
        );

        next_id
    }

    /// Create a buy order (Bid).
    ///
    /// Escrows `total_cost` of `payment_token` from `maker` into the marketplace contract.
    /// The maker must have approved the marketplace for `total_cost`.
    pub fn create_buy_order(
        e: Env,
        maker: Address,
        credit_token: Address,
        payment_token: Address,
        amount: i128,
        price_per_credit: i128,
    ) -> u64 {
        require_not_paused(&e);
        maker.require_auth();

        if amount <= 0 {
            panic!("amount must be positive");
        }
        if price_per_credit <= 0 {
            panic!("price must be positive");
        }

        let total_cost = calculate_cost(amount, price_per_credit);
        let contract_address = e.current_contract_address();

        // Escrow payment tokens from maker to marketplace
        token_transfer_from(
            &e,
            &payment_token,
            &contract_address,
            &maker,
            &contract_address,
            total_cost,
        );

        let next_id = read_order_count(&e).checked_add(1).expect("overflow");
        save_order_count(&e, next_id);

        let order = Order {
            id: next_id,
            maker: maker.clone(),
            side: OrderSide::Buy,
            credit_token: credit_token.clone(),
            payment_token: payment_token.clone(),
            amount,
            filled_amount: 0,
            price_per_credit,
            created_at: e.ledger().timestamp(),
            status: OrderStatus::Open,
        };

        save_order(&e, &order);
        add_maker_order(&e, &maker, next_id);

        e.events().publish(
            (EVENT_ORDER_CREATED,),
            (
                next_id,
                maker,
                OrderSide::Buy,
                credit_token,
                payment_token,
                amount,
                price_per_credit,
            ),
        );

        next_id
    }

    /// Fill an open order atomically (full or partial fill).
    ///
    /// - If filling a **Sell** order:
    ///   - Taker provides `payment_token` (approved beforehand).
    ///   - Taker receives `credit_token` from marketplace escrow.
    /// - If filling a **Buy** order:
    ///   - Taker provides `credit_token` (approved beforehand).
    ///   - Taker receives `payment_token` from marketplace escrow.
    pub fn fill_order(e: Env, taker: Address, order_id: u64, fill_amount: i128) {
        require_not_paused(&e);
        taker.require_auth();
        Self::internal_fill_order(&e, &taker, order_id, fill_amount);
    }

    /// Batch fill multiple orders in a single transaction.
    pub fn batch_fill_orders(e: Env, taker: Address, order_ids: Vec<u64>, fill_amounts: Vec<i128>) {
        require_not_paused(&e);
        taker.require_auth();

        if order_ids.len() != fill_amounts.len() {
            panic!("order_ids and fill_amounts length mismatch");
        }
        if order_ids.is_empty() {
            panic!("empty batch fill");
        }

        for i in 0..order_ids.len() {
            let order_id = order_ids.get(i).unwrap();
            let fill_amount = fill_amounts.get(i).unwrap();
            Self::internal_fill_order(&e, &taker, order_id, fill_amount);
        }
    }

    /// Cancel an open order and return any remaining escrowed tokens to the maker.
    pub fn cancel_order(e: Env, maker: Address, order_id: u64) {
        maker.require_auth();

        let mut order = read_order(&e, order_id).expect("order not found");
        if order.maker != maker {
            panic!("unauthorized: caller is not maker");
        }
        if order.status != OrderStatus::Open && order.status != OrderStatus::PartiallyFilled {
            panic!("order is not open for cancellation");
        }

        let remaining = order
            .amount
            .checked_sub(order.filled_amount)
            .expect("underflow");

        let contract_address = e.current_contract_address();

        match order.side {
            OrderSide::Sell => {
                // Refund remaining credit_token to maker
                if remaining > 0 {
                    token_transfer(
                        &e,
                        &order.credit_token,
                        &contract_address,
                        &maker,
                        remaining,
                    );
                }
            }
            OrderSide::Buy => {
                // Refund remaining unspent payment_token to maker
                let remaining_cost = calculate_cost(remaining, order.price_per_credit);
                if remaining_cost > 0 {
                    token_transfer(
                        &e,
                        &order.payment_token,
                        &contract_address,
                        &maker,
                        remaining_cost,
                    );
                }
            }
        }

        order.status = OrderStatus::Cancelled;
        save_order(&e, &order);
        remove_maker_order(&e, &maker, order.id);

        e.events()
            .publish((EVENT_ORDER_CANCELLED,), (order_id, maker));
    }

    /// Fetch an order by its ID.
    pub fn get_order(e: Env, order_id: u64) -> Option<Order> {
        read_order(&e, order_id)
    }

    /// Fetch all open order IDs for a specific maker.
    pub fn get_open_orders_by_maker(e: Env, maker: Address) -> Vec<u64> {
        read_maker_orders(&e, &maker)
    }

    /// Fetch a slice of orders for a token with pagination.
    pub fn get_orders_for_token(
        e: Env,
        credit_token: Address,
        start_id: u64,
        limit: u32,
    ) -> Vec<Order> {
        let total = read_order_count(&e);
        let mut results = Vec::new(&e);
        if limit == 0 || start_id > total || total == 0 {
            return results;
        }

        let mut current_id = start_id;
        while current_id <= total && results.len() < limit {
            if let Some(order) = read_order(&e, current_id) {
                if order.credit_token == credit_token {
                    results.push_back(order);
                }
            }
            current_id += 1;
        }
        results
    }
}
