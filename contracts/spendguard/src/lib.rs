//! SpendGuard vault contract.
//!
//! The human owner deposits NEAR and sets a policy. The authorized AI agent
//! may spend from the vault only through `agent_spend` / `agent_call`, and
//! only when the shared [`spendguard_policy`] engine accepts the request.
//!
//! The agent is expected to hold a Function-Call access key scoped to this
//! contract (method names `agent_spend`, `agent_call`) — never a full-access
//! key. See the README for the limits of that key.

use near_sdk::json_types::U128;
use near_sdk::{
    env, near, require, AccountId, Gas, NearToken, PanicOnDefault, Promise,
};
use spendguard_policy::{
    day_index, decide, simulate as policy_simulate, DailyWindow, Decision, Policy,
    SpendKind, SpendRequest, DAY_NS, MAX_ALLOWLIST, YOCTO_NEAR,
};

/// Default storage reserve: 0.05 NEAR stays in the contract so it is not
/// depleted by agent spends (enough for state + a few receipts).
const DEFAULT_STORAGE_RESERVE: u128 = YOCTO_NEAR / 20;

/// Gas attached to a forwarded `agent_call` if the caller does not specify any.
const DEFAULT_FORWARD_GAS: Gas = Gas::from_tgas(30);

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    owner: AccountId,
    agent: AccountId,
    per_tx_max: u128,
    daily_max: u128,
    allowed_receivers: Vec<AccountId>,
    allowed_methods: Vec<String>,
    paused: bool,
    day_index: u64,
    daily_spent: u128,
    storage_reserve: u128,
}

#[near(serializers = [json])]
pub struct PolicyView {
    pub owner: AccountId,
    pub agent: AccountId,
    pub per_tx_max: U128,
    pub daily_max: U128,
    pub allowed_receivers: Vec<AccountId>,
    pub allowed_methods: Vec<String>,
    pub paused: bool,
    pub storage_reserve: U128,
}

#[near(serializers = [json])]
pub struct DailyUsageView {
    pub day_index: u64,
    pub spent: U128,
    pub remaining: U128,
    pub daily_max: U128,
    pub seconds_until_reset: u64,
}

#[near(serializers = [json])]
pub struct DecisionView {
    pub allowed: bool,
    pub code: String,
    pub reason: String,
    pub amount: U128,
    pub per_tx_max: U128,
    pub daily_remaining: U128,
    pub daily_spent: U128,
    pub daily_max: U128,
}

impl From<Decision> for DecisionView {
    fn from(d: Decision) -> Self {
        Self {
            allowed: d.allowed,
            code: d.code,
            reason: d.reason,
            amount: U128(d.amount),
            per_tx_max: U128(d.per_tx_max),
            daily_remaining: U128(d.daily_remaining),
            daily_spent: U128(d.daily_spent),
            daily_max: U128(d.daily_max),
        }
    }
}

#[near]
impl Contract {
    /// Deploy-time constructor. Starts fail-closed: limits are 0 and both
    /// allowlists are empty, so the agent cannot spend until the owner
    /// calls `set_policy`.
    #[init]
    pub fn new(owner: AccountId, agent: AccountId) -> Self {
        Self {
            owner,
            agent,
            per_tx_max: 0,
            daily_max: 0,
            allowed_receivers: Vec::new(),
            allowed_methods: Vec::new(),
            paused: false,
            day_index: 0,
            daily_spent: 0,
            storage_reserve: DEFAULT_STORAGE_RESERVE,
        }
    }

    // ------------------------------------------------------------------
    // Owner: policy
    // ------------------------------------------------------------------

    /// Replace limits and both allowlists in one call. Empty lists stay
    /// fail-closed (no receivers / no function calls).
    pub fn set_policy(
        &mut self,
        per_tx_max: U128,
        daily_max: U128,
        allowed_receivers: Vec<AccountId>,
        allowed_methods: Vec<String>,
    ) {
        self.assert_owner();
        require!(
            per_tx_max.0 <= daily_max.0,
            "per-tx max cannot exceed daily max"
        );
        require!(
            allowed_receivers.len() <= MAX_ALLOWLIST,
            "too many receivers (max 64)"
        );
        require!(
            allowed_methods.len() <= MAX_ALLOWLIST,
            "too many methods (max 64)"
        );
        for m in &allowed_methods {
            require!(!m.is_empty(), "method name must not be empty");
        }
        self.per_tx_max = per_tx_max.0;
        self.daily_max = daily_max.0;
        self.allowed_receivers = allowed_receivers;
        self.allowed_methods = allowed_methods;
        self.emit(
            "policy_set",
            &format!(
                "{{\"per_tx_max\":\"{}\",\"daily_max\":\"{}\"}}",
                self.per_tx_max, self.daily_max
            ),
        );
    }

    pub fn set_agent(&mut self, agent: AccountId) {
        self.assert_owner();
        self.agent = agent;
    }

    pub fn set_limits(&mut self, per_tx_max: U128, daily_max: U128) {
        self.assert_owner();
        require!(
            per_tx_max.0 <= daily_max.0,
            "per-tx max cannot exceed daily max"
        );
        self.per_tx_max = per_tx_max.0;
        self.daily_max = daily_max.0;
    }

    pub fn add_receiver(&mut self, receiver: AccountId) {
        self.assert_owner();
        require!(
            self.allowed_receivers.len() < MAX_ALLOWLIST,
            "too many receivers (max 64)"
        );
        if !self.allowed_receivers.contains(&receiver) {
            self.allowed_receivers.push(receiver);
        }
    }

    pub fn remove_receiver(&mut self, receiver: AccountId) {
        self.assert_owner();
        self.allowed_receivers.retain(|r| r != &receiver);
    }

    pub fn add_method(&mut self, method: String) {
        self.assert_owner();
        require!(!method.is_empty(), "method name must not be empty");
        require!(
            self.allowed_methods.len() < MAX_ALLOWLIST,
            "too many methods (max 64)"
        );
        if !self.allowed_methods.contains(&method) {
            self.allowed_methods.push(method);
        }
    }

    pub fn remove_method(&mut self, method: String) {
        self.assert_owner();
        self.allowed_methods.retain(|m| m != &method);
    }

    pub fn pause(&mut self) {
        self.assert_owner();
        self.paused = true;
        self.emit("paused", "{}");
    }

    pub fn unpause(&mut self) {
        self.assert_owner();
        self.paused = false;
        self.emit("unpaused", "{}");
    }

    pub fn set_storage_reserve(&mut self, amount: U128) {
        self.assert_owner();
        self.storage_reserve = amount.0;
    }

    /// Anyone may fund the vault. The deposit stays under the policy.
    #[payable]
    pub fn deposit(&mut self) {
        let attached = env::attached_deposit().as_yoctonear();
        require!(attached > 0, "attach a positive deposit");
        self.emit(
            "deposited",
            &format!(
                "{{\"from\":\"{}\",\"amount\":\"{}\"}}",
                env::predecessor_account_id(),
                attached
            ),
        );
    }

    pub fn owner_withdraw(&mut self, to: AccountId, amount: U128) -> Promise {
        self.assert_owner();
        require!(amount.0 > 0, "amount must be positive");
        let available = self.vault_available();
        require!(
            amount.0 <= available,
            "insufficient vault balance (after storage reserve)"
        );
        self.emit(
            "owner_withdraw",
            &format!("{{\"to\":\"{to}\",\"amount\":\"{}\"}}", amount.0),
        );
        Promise::new(to).transfer(NearToken::from_yoctonear(amount.0))
    }

    // ------------------------------------------------------------------
    // Agent: spends (intended to be the only methods on a FC access key)
    // ------------------------------------------------------------------

    /// Native NEAR transfer to an allowlisted receiver, if the policy agrees.
    pub fn agent_spend(&mut self, receiver: AccountId, amount: U128, memo: Option<String>) -> Promise {
        self.execute(receiver.clone(), amount.0, SpendKind::Transfer);
        let memo_s = memo.unwrap_or_default();
        self.emit(
            "spend_approved",
            &format!(
                "{{\"receiver\":\"{receiver}\",\"amount\":\"{}\",\"kind\":\"transfer\",\"memo\":\"{}\"}}",
                amount.0,
                escape(&memo_s)
            ),
        );
        Promise::new(receiver).transfer(NearToken::from_yoctonear(amount.0))
    }

    /// Function call on an allowlisted contract, with an optional attached
    /// deposit that also counts against the limits.
    pub fn agent_call(
        &mut self,
        receiver: AccountId,
        method: String,
        args: String,
        amount: U128,
        gas_tgas: Option<u64>,
    ) -> Promise {
        self.execute(
            receiver.clone(),
            amount.0,
            SpendKind::FunctionCall {
                method: method.clone(),
            },
        );
        self.emit(
            "spend_approved",
            &format!(
                "{{\"receiver\":\"{receiver}\",\"amount\":\"{}\",\"kind\":\"call\",\"method\":\"{}\"}}",
                amount.0,
                escape(&method)
            ),
        );
        let gas = Gas::from_tgas(gas_tgas.unwrap_or(DEFAULT_FORWARD_GAS.as_tgas()));
        Promise::new(receiver).function_call(
            method,
            args.into_bytes(),
            NearToken::from_yoctonear(amount.0),
            gas,
        )
    }

    // ------------------------------------------------------------------
    // Views
    // ------------------------------------------------------------------

    pub fn get_policy(&self) -> PolicyView {
        PolicyView {
            owner: self.owner.clone(),
            agent: self.agent.clone(),
            per_tx_max: U128(self.per_tx_max),
            daily_max: U128(self.daily_max),
            allowed_receivers: self.allowed_receivers.clone(),
            allowed_methods: self.allowed_methods.clone(),
            paused: self.paused,
            storage_reserve: U128(self.storage_reserve),
        }
    }

    pub fn get_daily_usage(&self) -> DailyUsageView {
        let now = env::block_timestamp();
        let today = day_index(now);
        let (spent, remaining) = if self.day_index == today {
            (
                self.daily_spent,
                self.daily_max.saturating_sub(self.daily_spent),
            )
        } else {
            (0, self.daily_max)
        };
        let elapsed_in_day = now % DAY_NS;
        let seconds_until_reset = (DAY_NS - elapsed_in_day) / 1_000_000_000;
        DailyUsageView {
            day_index: today,
            spent: U128(spent),
            remaining: U128(remaining),
            daily_max: U128(self.daily_max),
            seconds_until_reset,
        }
    }

    pub fn get_owner(&self) -> AccountId {
        self.owner.clone()
    }

    pub fn get_agent(&self) -> AccountId {
        self.agent.clone()
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn get_vault_available(&self) -> U128 {
        U128(self.vault_available())
    }

    /// Dry-run. Agents should call this (view, no gas key required) before
    /// signing `agent_spend` / `agent_call`. Does not change state.
    pub fn simulate(
        &self,
        caller: AccountId,
        receiver: AccountId,
        method: Option<String>,
        amount: U128,
    ) -> DecisionView {
        let (policy, window, now, req) =
            self.as_request(&caller, &receiver, method, amount.0, self.vault_available());
        DecisionView::from(policy_simulate(&policy, &window, now, &req))
    }
}

impl Contract {
    fn assert_owner(&self) {
        require!(
            env::predecessor_account_id() == self.owner,
            "only the owner can do this"
        );
    }

    fn vault_available(&self) -> u128 {
        let bal = env::account_balance().as_yoctonear();
        bal.saturating_sub(self.storage_reserve)
    }

    fn as_policy(&self) -> Policy {
        Policy {
            agent: self.agent.to_string(),
            per_tx_max: self.per_tx_max,
            daily_max: self.daily_max,
            allowed_receivers: self
                .allowed_receivers
                .iter()
                .map(|a| a.to_string())
                .collect(),
            allowed_methods: self.allowed_methods.clone(),
            paused: self.paused,
        }
    }

    fn as_window(&self, now_ns: u64) -> DailyWindow {
        DailyWindow {
            day_index: if self.day_index == day_index(now_ns) {
                self.day_index
            } else {
                // A stale window is still passed in; spent_at() returns 0
                // when the day does not match, which is what we want.
                self.day_index
            },
            spent: self.daily_spent,
        }
    }

    fn as_request<'a>(
        &self,
        caller: &'a AccountId,
        receiver: &'a AccountId,
        method: Option<String>,
        amount: u128,
        vault_available: u128,
    ) -> (Policy, DailyWindow, u64, SpendRequest<'a>) {
        let now = env::block_timestamp();
        let policy = self.as_policy();
        let window = self.as_window(now);
        let kind = match method {
            Some(m) => SpendKind::FunctionCall { method: m },
            None => SpendKind::Transfer,
        };
        let caller_s = caller.as_str();
        let receiver_s = receiver.as_str();
        // SpendRequest holds &str; AccountId deref to str is stable.
        let req = SpendRequest {
            caller: caller_s,
            receiver: receiver_s,
            amount,
            kind,
            vault_available,
        };
        (policy, window, now, req)
    }

    /// Shared path for `agent_spend` and `agent_call`. Panics with a
    /// structured reason on deny (the log is also emitted as a NEP-297 event).
    fn execute(&mut self, receiver: AccountId, amount: u128, kind: SpendKind) {
        let caller = env::predecessor_account_id();
        let now = env::block_timestamp();
        let policy = self.as_policy();
        let window = self.as_window(now);
        let method_for_log = kind.method().unwrap_or("").to_string();
        let req = SpendRequest {
            caller: caller.as_str(),
            receiver: receiver.as_str(),
            amount,
            kind,
            vault_available: self.vault_available(),
        };
        match decide(&policy, &window, now, &req) {
            Ok(()) => {
                if self.day_index != day_index(now) {
                    self.day_index = day_index(now);
                    self.daily_spent = 0;
                }
                self.daily_spent = self.daily_spent.saturating_add(amount);
            }
            Err(why) => {
                self.emit(
                    "spend_denied",
                    &format!(
                        "{{\"code\":\"{}\",\"reason\":\"{}\",\"caller\":\"{caller}\",\"receiver\":\"{receiver}\",\"amount\":\"{amount}\",\"method\":\"{}\"}}",
                        why.code(),
                        escape(&why.to_string()),
                        escape(&method_for_log)
                    ),
                );
                env::panic_str(&why.to_string());
            }
        }
    }

    fn emit(&self, event: &str, data: &str) {
        env::log_str(&format!(
            "EVENT_JSON:{{\"standard\":\"spendguard\",\"version\":\"1.0.0\",\"event\":\"{event}\",\"data\":[{data}]}}"
        ));
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
#[allow(unused_must_use)]
mod tests {
    use super::*;
    use near_sdk::test_utils::{accounts, VMContextBuilder};
    use near_sdk::{testing_env, AccountId, NearToken};

    fn owner() -> AccountId {
        accounts(0)
    }
    fn agent() -> AccountId {
        accounts(1)
    }
    fn merchant() -> AccountId {
        accounts(2)
    }
    fn attacker() -> AccountId {
        accounts(3)
    }

    fn context(
        predecessor: AccountId,
        deposit: u128,
        balance: u128,
        timestamp_ns: u64,
    ) -> VMContextBuilder {
        let mut b = VMContextBuilder::new();
        b.predecessor_account_id(predecessor)
            .attached_deposit(NearToken::from_yoctonear(deposit))
            .account_balance(NearToken::from_yoctonear(balance))
            .block_timestamp(timestamp_ns);
        b
    }

    fn env_as(predecessor: AccountId, balance: u128, ts: u64) {
        testing_env!(context(predecessor, 0, balance, ts).build());
    }

    fn funded_vault() -> Contract {
        // 10 NEAR on the account, 0.05 reserved → 9.95 available.
        env_as(owner(), near_sdk_near(10), DAY_NS / 2);
        let mut c = Contract::new(owner(), agent());
        c.set_policy(
            U128(near_sdk_near(1)),
            U128(near_sdk_near(2)),
            vec![merchant()],
            vec!["api_charge".to_string()],
        );
        c
    }

    fn near_sdk_near(n: u128) -> u128 {
        n * YOCTO_NEAR
    }
    fn milli(n: u128) -> u128 {
        n * YOCTO_NEAR / 1000
    }

    #[test]
    fn starts_fail_closed() {
        env_as(owner(), near_sdk_near(10), 0);
        let c = Contract::new(owner(), agent());
        let d = c.simulate(agent(), merchant(), None, U128(1));
        assert!(!d.allowed);
        // Fresh contract: empty allowlist is deny-all (limits are also 0).
        assert_eq!(d.code, "ReceiverNotAllowed");
    }

    #[test]
    fn owner_sets_policy_agent_spends_under_limit() {
        let mut c = funded_vault();
        env_as(agent(), near_sdk_near(10), DAY_NS / 2);
        let d = c.simulate(agent(), merchant(), None, U128(milli(400)));
        assert!(d.allowed, "{}", d.reason);
        c.agent_spend(merchant(), U128(milli(400)), Some("api fee".into()));
        let usage = c.get_daily_usage();
        assert_eq!(usage.spent.0, milli(400));
        assert_eq!(usage.remaining.0, milli(1600));
    }

    #[test]
    fn over_per_tx_panics() {
        let mut c = funded_vault();
        env_as(agent(), near_sdk_near(10), DAY_NS / 2);
        let d = c.simulate(agent(), merchant(), None, U128(milli(1500)));
        assert!(!d.allowed);
        assert_eq!(d.code, "PerTxLimitExceeded");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            c.agent_spend(merchant(), U128(milli(1500)), None);
        }));
        assert!(result.is_err());
        // State must be unchanged after a rejected spend.
        assert_eq!(c.get_daily_usage().spent.0, 0);
    }

    #[test]
    fn receiver_not_allowed_panics() {
        let mut c = funded_vault();
        env_as(agent(), near_sdk_near(10), DAY_NS / 2);
        let d = c.simulate(agent(), attacker(), None, U128(milli(100)));
        assert_eq!(d.code, "ReceiverNotAllowed");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            c.agent_spend(attacker(), U128(milli(100)), None);
        }));
        assert!(result.is_err());
    }

    #[test]
    fn unauthorized_caller_rejected() {
        let mut c = funded_vault();
        env_as(attacker(), near_sdk_near(10), DAY_NS / 2);
        let d = c.simulate(attacker(), merchant(), None, U128(milli(100)));
        assert_eq!(d.code, "UnauthorizedAgent");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            c.agent_spend(merchant(), U128(milli(100)), None);
        }));
        assert!(result.is_err());
    }

    #[test]
    fn only_owner_sets_policy() {
        let mut c = funded_vault();
        env_as(agent(), near_sdk_near(10), DAY_NS / 2);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            c.set_limits(U128(near_sdk_near(9)), U128(near_sdk_near(9)));
        }));
        assert!(result.is_err());
    }

    #[test]
    fn daily_limit_and_utc_reset() {
        let mut c = funded_vault();
        env_as(agent(), near_sdk_near(10), DAY_NS / 2);
        c.agent_spend(merchant(), U128(near_sdk_near(1)), None);
        c.agent_spend(merchant(), U128(near_sdk_near(1)), None);
        let d = c.simulate(agent(), merchant(), None, U128(1));
        assert_eq!(d.code, "DailyLimitExceeded");

        // Next UTC day, the window is fresh.
        env_as(agent(), near_sdk_near(10) - near_sdk_near(2), DAY_NS + DAY_NS / 2);
        let d = c.simulate(agent(), merchant(), None, U128(near_sdk_near(1)));
        assert!(d.allowed, "{}", d.reason);
        assert_eq!(d.daily_spent.0, 0);
        c.agent_spend(merchant(), U128(near_sdk_near(1)), None);
        assert_eq!(c.get_daily_usage().spent.0, near_sdk_near(1));
    }

    #[test]
    fn pause_blocks_agent() {
        let mut c = funded_vault();
        env_as(owner(), near_sdk_near(10), DAY_NS / 2);
        c.pause();
        env_as(agent(), near_sdk_near(10), DAY_NS / 2);
        assert_eq!(
            c.simulate(agent(), merchant(), None, U128(milli(100))).code,
            "Paused"
        );
    }

    #[test]
    fn agent_call_requires_method_allowlist() {
        let c = funded_vault();
        env_as(agent(), near_sdk_near(10), DAY_NS / 2);
        let ok = c.simulate(
            agent(),
            merchant(),
            Some("api_charge".into()),
            U128(milli(200)),
        );
        assert!(ok.allowed, "{}", ok.reason);
        let no = c.simulate(agent(), merchant(), Some("drain".into()), U128(milli(200)));
        assert_eq!(no.code, "MethodNotAllowed");
    }

    #[test]
    fn deposit_then_withdraw() {
        env_as(owner(), near_sdk_near(10), DAY_NS / 2);
        let mut c = Contract::new(owner(), agent());
        testing_env!(context(owner(), near_sdk_near(2), near_sdk_near(10), DAY_NS / 2).build());
        c.deposit();
        // After deposit, test env does not automatically bump account_balance;
        // withdraw is tested against the configured balance minus reserve.
        env_as(owner(), near_sdk_near(10), DAY_NS / 2);
        c.owner_withdraw(owner(), U128(near_sdk_near(1)));
    }
}
