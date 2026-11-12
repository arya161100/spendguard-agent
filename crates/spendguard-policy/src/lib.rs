//! SpendGuard policy engine.
//!
//! Pure Rust: no blockchain SDK, no I/O, no clocks of its own. The NEAR
//! contract and the local CLI both call [`decide`] so a judge can replay the
//! same rules offline that the on-chain vault will enforce.
//!
//! Fail-closed defaults:
//! - `per_tx_max == 0` or `daily_max == 0` rejects every spend.
//! - An empty receiver allowlist rejects every spend.
//! - An empty method allowlist rejects every function call (native
//!   transfers still go through if the receiver and limits pass).

use std::fmt;

/// One NEAR, in yoctoNEAR (10^24).
pub const YOCTO_NEAR: u128 = 1_000_000_000_000_000_000_000_000;

/// Length of a UTC day, in nanoseconds (NEAR block timestamps are ns).
pub const DAY_NS: u64 = 86_400_000_000_000;

/// Hard cap on allowlist entries, to keep contract storage bounded.
pub const MAX_ALLOWLIST: usize = 64;

/// Convert a whole-NEAR amount to yoctoNEAR.
pub fn near(n: u128) -> u128 {
    n.checked_mul(YOCTO_NEAR)
        .expect("NEAR amount overflows yoctoNEAR")
}

/// Fractional helper: `milli_near(400)` is 0.4 NEAR.
pub fn milli_near(n: u128) -> u128 {
    n.checked_mul(YOCTO_NEAR / 1000)
        .expect("milliNEAR amount overflows yoctoNEAR")
}

/// UTC day index from a NEAR block timestamp (nanoseconds).
pub fn day_index(timestamp_ns: u64) -> u64 {
    timestamp_ns / DAY_NS
}

/// Human-set spending rules. Empty allowlists are deny-all (fail-closed).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Policy {
    pub agent: String,
    pub per_tx_max: u128,
    pub daily_max: u128,
    pub allowed_receivers: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub paused: bool,
}

impl Policy {
    /// A fresh policy: no agent can spend until the owner fills this in.
    pub fn fail_closed(agent: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            per_tx_max: 0,
            daily_max: 0,
            allowed_receivers: Vec::new(),
            allowed_methods: Vec::new(),
            paused: false,
        }
    }
}

/// Running total for the current UTC day.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DailyWindow {
    pub day_index: u64,
    pub spent: u128,
}

impl DailyWindow {
    pub fn spent_at(&self, now_ns: u64) -> u128 {
        if self.day_index == day_index(now_ns) {
            self.spent
        } else {
            0
        }
    }

    pub fn remaining(&self, daily_max: u128, now_ns: u64) -> u128 {
        daily_max.saturating_sub(self.spent_at(now_ns))
    }

    /// Record a successful spend, rolling the window forward if the day changed.
    pub fn record_success(&mut self, now_ns: u64, amount: u128) {
        let today = day_index(now_ns);
        if self.day_index != today {
            self.day_index = today;
            self.spent = 0;
        }
        self.spent = self.spent.saturating_add(amount);
    }
}

/// What the agent is trying to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpendKind {
    /// Native NEAR transfer (`agent_spend`).
    Transfer,
    /// Attached-deposit function call on an allowlisted contract (`agent_call`).
    FunctionCall { method: String },
}

impl SpendKind {
    pub fn method(&self) -> Option<&str> {
        match self {
            SpendKind::Transfer => None,
            SpendKind::FunctionCall { method } => Some(method.as_str()),
        }
    }
}

/// One attempted spend, already parsed into typed fields.
#[derive(Clone, Debug)]
pub struct SpendRequest<'a> {
    pub caller: &'a str,
    pub receiver: &'a str,
    pub amount: u128,
    pub kind: SpendKind,
    /// Vault balance minus the storage reserve. The policy will not
    /// approve a spend that would dip into reserved funds.
    pub vault_available: u128,
}

/// Why a spend was rejected. `Allowed` is represented by `Ok(())`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DenyReason {
    Paused,
    UnauthorizedAgent { caller: String, agent: String },
    AmountZero,
    PerTxLimitExceeded { amount: u128, max: u128 },
    DailyLimitExceeded { amount: u128, remaining: u128 },
    ReceiverNotAllowed { receiver: String },
    MethodNotAllowed { method: String },
    InsufficientVault { needed: u128, available: u128 },
}

impl DenyReason {
    /// Stable machine-readable code, used by the CLI, tests, and NEP-297 events.
    pub fn code(&self) -> &'static str {
        match self {
            DenyReason::Paused => "Paused",
            DenyReason::UnauthorizedAgent { .. } => "UnauthorizedAgent",
            DenyReason::AmountZero => "AmountZero",
            DenyReason::PerTxLimitExceeded { .. } => "PerTxLimitExceeded",
            DenyReason::DailyLimitExceeded { .. } => "DailyLimitExceeded",
            DenyReason::ReceiverNotAllowed { .. } => "ReceiverNotAllowed",
            DenyReason::MethodNotAllowed { .. } => "MethodNotAllowed",
            DenyReason::InsufficientVault { .. } => "InsufficientVault",
        }
    }
}

impl fmt::Display for DenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DenyReason::Paused => {
                write!(f, "Paused: the owner has paused the agent")
            }
            DenyReason::UnauthorizedAgent { caller, agent } => {
                write!(
                    f,
                    "UnauthorizedAgent: caller '{caller}' is not the authorized agent '{agent}'"
                )
            }
            DenyReason::AmountZero => {
                write!(f, "AmountZero: spend amount must be greater than zero")
            }
            DenyReason::PerTxLimitExceeded { amount, max } => {
                write!(
                    f,
                    "PerTxLimitExceeded: amount {} exceeds per-tx max {}",
                    fmt_near(*amount),
                    fmt_near(*max)
                )
            }
            DenyReason::DailyLimitExceeded { amount, remaining } => {
                write!(
                    f,
                    "DailyLimitExceeded: amount {} exceeds daily remaining {}",
                    fmt_near(*amount),
                    fmt_near(*remaining)
                )
            }
            DenyReason::ReceiverNotAllowed { receiver } => {
                write!(
                    f,
                    "ReceiverNotAllowed: '{receiver}' is not on the owner allowlist"
                )
            }
            DenyReason::MethodNotAllowed { method } => {
                write!(
                    f,
                    "MethodNotAllowed: '{method}' is not on the owner allowlist"
                )
            }
            DenyReason::InsufficientVault { needed, available } => {
                write!(
                    f,
                    "InsufficientVault: need {} but vault has {} available (after storage reserve)",
                    fmt_near(*needed),
                    fmt_near(*available)
                )
            }
        }
    }
}

/// Result of a dry-run (`simulate`) or of a live decision.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Decision {
    pub allowed: bool,
    pub code: String,
    pub reason: String,
    pub amount: u128,
    pub per_tx_max: u128,
    pub daily_remaining: u128,
    pub daily_spent: u128,
    pub daily_max: u128,
}

impl Decision {
    pub fn allow(req: &SpendRequest<'_>, policy: &Policy, daily_spent: u128, daily_remaining: u128) -> Self {
        Self {
            allowed: true,
            code: "Allowed".to_string(),
            reason: "Allowed: all policy checks passed".to_string(),
            amount: req.amount,
            per_tx_max: policy.per_tx_max,
            daily_remaining,
            daily_spent,
            daily_max: policy.daily_max,
        }
    }

    pub fn deny(
        req: &SpendRequest<'_>,
        policy: &Policy,
        daily_spent: u128,
        daily_remaining: u128,
        why: &DenyReason,
    ) -> Self {
        Self {
            allowed: false,
            code: why.code().to_string(),
            reason: why.to_string(),
            amount: req.amount,
            per_tx_max: policy.per_tx_max,
            daily_remaining,
            daily_spent,
            daily_max: policy.daily_max,
        }
    }
}

/// Core entry point: accept or reject a spend. Does not mutate state.
///
/// On `Ok(())` the caller should transfer funds and then
/// [`DailyWindow::record_success`].
pub fn decide(
    policy: &Policy,
    daily: &DailyWindow,
    now_ns: u64,
    req: &SpendRequest<'_>,
) -> Result<(), DenyReason> {
    if policy.paused {
        return Err(DenyReason::Paused);
    }
    if req.caller != policy.agent {
        return Err(DenyReason::UnauthorizedAgent {
            caller: req.caller.to_string(),
            agent: policy.agent.clone(),
        });
    }
    if req.amount == 0 {
        return Err(DenyReason::AmountZero);
    }
    // Authorization before arithmetic: a spend to a stranger, or a call to a
    // method the owner never approved, is denied even if the amount would
    // also have exceeded a cap. This is the prompt-injection case.
    if !is_allowed(&policy.allowed_receivers, req.receiver) {
        return Err(DenyReason::ReceiverNotAllowed {
            receiver: req.receiver.to_string(),
        });
    }
    if let SpendKind::FunctionCall { method } = &req.kind {
        if !is_allowed(&policy.allowed_methods, method) {
            return Err(DenyReason::MethodNotAllowed {
                method: method.clone(),
            });
        }
    }
    // Fail-closed: a zero limit means "no spending yet".
    if req.amount > policy.per_tx_max {
        return Err(DenyReason::PerTxLimitExceeded {
            amount: req.amount,
            max: policy.per_tx_max,
        });
    }
    let remaining = daily.remaining(policy.daily_max, now_ns);
    if req.amount > remaining {
        return Err(DenyReason::DailyLimitExceeded {
            amount: req.amount,
            remaining,
        });
    }
    if req.amount > req.vault_available {
        return Err(DenyReason::InsufficientVault {
            needed: req.amount,
            available: req.vault_available,
        });
    }
    Ok(())
}

/// Dry-run wrapper used by agents before they sign, and by the CLI.
pub fn simulate(
    policy: &Policy,
    daily: &DailyWindow,
    now_ns: u64,
    req: &SpendRequest<'_>,
) -> Decision {
    let spent = daily.spent_at(now_ns);
    let remaining = daily.remaining(policy.daily_max, now_ns);
    match decide(policy, daily, now_ns, req) {
        Ok(()) => Decision::allow(req, policy, spent, remaining),
        Err(why) => Decision::deny(req, policy, spent, remaining, &why),
    }
}

fn is_allowed(list: &[String], item: &str) -> bool {
    // Empty = deny all. Comparison is exact (NEAR account ids are case-sensitive
    // and already lowercase by convention).
    !list.is_empty() && list.iter().any(|e| e == item)
}

/// Format yoctoNEAR as a short human string, e.g. `1.5 NEAR`.
pub fn fmt_near(yocto: u128) -> String {
    if yocto == 0 {
        return "0 NEAR".to_string();
    }
    let whole = yocto / YOCTO_NEAR;
    let rest = yocto % YOCTO_NEAR;
    if rest == 0 {
        return format!("{whole} NEAR");
    }
    // Up to 6 decimal places, trimmed.
    let mut frac = format!("{rest:024}");
    frac.truncate(6);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        format!("{whole} NEAR")
    } else {
        format!("{whole}.{frac} NEAR")
    }
}

/// Parse amounts like `1`, `1N`, `1.5`, `0.4 NEAR`, or a raw yocto integer
/// of 20+ digits. Used by the CLI only; the contract takes `U128` yoctoNEAR.
pub fn parse_near(input: &str) -> Result<u128, String> {
    let s = input
        .trim()
        .trim_end_matches("NEAR")
        .trim_end_matches("near")
        .trim_end_matches('N')
        .trim_end_matches('n')
        .trim()
        .replace('_', "");
    if s.is_empty() {
        return Err("empty amount".into());
    }
    if s.chars().all(|c| c.is_ascii_digit()) && s.len() >= 20 {
        return s.parse::<u128>().map_err(|e| e.to_string());
    }
    if let Some((whole, frac)) = s.split_once('.') {
        if whole.is_empty() || !whole.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!("invalid amount '{input}'"));
        }
        if frac.is_empty() || !frac.chars().all(|c| c.is_ascii_digit()) || frac.len() > 24 {
            return Err(format!("invalid fractional amount '{input}'"));
        }
        let whole_n: u128 = whole.parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
        let mut frac_pad = frac.to_string();
        while frac_pad.len() < 24 {
            frac_pad.push('0');
        }
        let frac_n: u128 = frac_pad.parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
        let whole_yocto = whole_n
            .checked_mul(YOCTO_NEAR)
            .ok_or_else(|| format!("amount '{input}' overflows"))?;
        whole_yocto
            .checked_add(frac_n)
            .ok_or_else(|| format!("amount '{input}' overflows"))
    } else if s.chars().all(|c| c.is_ascii_digit()) {
        let n: u128 = s.parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
        n.checked_mul(YOCTO_NEAR)
            .ok_or_else(|| format!("amount '{input}' overflows"))
    } else {
        Err(format!("invalid amount '{input}'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            agent: "agent.testnet".into(),
            per_tx_max: near(1),
            daily_max: near(2),
            allowed_receivers: vec!["merchant.testnet".into()],
            allowed_methods: vec!["api_charge".into()],
            paused: false,
        }
    }

    fn req<'a>(amount: u128, kind: SpendKind) -> SpendRequest<'a> {
        SpendRequest {
            caller: "agent.testnet",
            receiver: "merchant.testnet",
            amount,
            kind,
            vault_available: near(10),
        }
    }

    const NOON_DAY_0: u64 = DAY_NS / 2;
    const NOON_DAY_1: u64 = DAY_NS + DAY_NS / 2;

    fn daily() -> DailyWindow {
        DailyWindow {
            day_index: 0,
            spent: 0,
        }
    }

    #[test]
    fn under_limit_transfer_allowed() {
        let d = decide(&policy(), &daily(), NOON_DAY_0, &req(milli_near(400), SpendKind::Transfer));
        assert_eq!(d, Ok(()));
    }

    #[test]
    fn under_limit_function_call_allowed() {
        let d = decide(
            &policy(),
            &daily(),
            NOON_DAY_0,
            &req(milli_near(200), SpendKind::FunctionCall { method: "api_charge".into() }),
        );
        assert_eq!(d, Ok(()));
    }

    #[test]
    fn over_per_tx_rejected() {
        let err = decide(&policy(), &daily(), NOON_DAY_0, &req(milli_near(1500), SpendKind::Transfer))
            .unwrap_err();
        assert_eq!(err.code(), "PerTxLimitExceeded");
        match err {
            DenyReason::PerTxLimitExceeded { amount, max } => {
                assert_eq!(amount, milli_near(1500));
                assert_eq!(max, near(1));
            }
            other => panic!("wrong reason: {other:?}"),
        }
    }

    #[test]
    fn exact_per_tx_max_allowed() {
        assert_eq!(
            decide(&policy(), &daily(), NOON_DAY_0, &req(near(1), SpendKind::Transfer)),
            Ok(())
        );
    }

    #[test]
    fn daily_limit_accumulated() {
        let mut w = daily();
        // 0.8 + 0.8 + 0.4 = 2.0 on the nose, then a further 0.1 fails.
        for amt in [milli_near(800), milli_near(800), milli_near(400)] {
            decide(&policy(), &w, NOON_DAY_0, &req(amt, SpendKind::Transfer)).unwrap();
            w.record_success(NOON_DAY_0, amt);
        }
        assert_eq!(w.spent, near(2));
        let err = decide(
            &policy(),
            &w,
            NOON_DAY_0,
            &req(milli_near(100), SpendKind::Transfer),
        )
        .unwrap_err();
        assert_eq!(err.code(), "DailyLimitExceeded");
    }

    #[test]
    fn daily_window_resets_next_utc_day() {
        let mut w = daily();
        decide(&policy(), &w, NOON_DAY_0, &req(near(1), SpendKind::Transfer)).unwrap();
        w.record_success(NOON_DAY_0, near(1));
        assert_eq!(w.spent_at(NOON_DAY_0), near(1));
        // Next UTC day: spent drops to 0, a 1 NEAR spend is allowed again.
        assert_eq!(w.spent_at(NOON_DAY_1), 0);
        assert_eq!(
            decide(&policy(), &w, NOON_DAY_1, &req(near(1), SpendKind::Transfer)),
            Ok(())
        );
        w.record_success(NOON_DAY_1, near(1));
        assert_eq!(w.day_index, 1);
        assert_eq!(w.spent, near(1));
    }

    #[test]
    fn receiver_not_on_allowlist() {
        let mut r = req(milli_near(100), SpendKind::Transfer);
        r.receiver = "attacker.near";
        let err = decide(&policy(), &daily(), NOON_DAY_0, &r).unwrap_err();
        assert_eq!(err.code(), "ReceiverNotAllowed");
        assert!(err.to_string().contains("attacker.near"));
    }

    #[test]
    fn empty_receiver_allowlist_is_fail_closed() {
        let mut p = policy();
        p.allowed_receivers.clear();
        let err = decide(&p, &daily(), NOON_DAY_0, &req(milli_near(100), SpendKind::Transfer))
            .unwrap_err();
        assert_eq!(err.code(), "ReceiverNotAllowed");
    }

    #[test]
    fn method_not_on_allowlist() {
        let err = decide(
            &policy(),
            &daily(),
            NOON_DAY_0,
            &req(
                milli_near(100),
                SpendKind::FunctionCall {
                    method: "drain".into(),
                },
            ),
        )
        .unwrap_err();
        assert_eq!(err.code(), "MethodNotAllowed");
    }

    #[test]
    fn empty_method_allowlist_blocks_calls_but_not_transfers() {
        let mut p = policy();
        p.allowed_methods.clear();
        assert_eq!(
            decide(&p, &daily(), NOON_DAY_0, &req(milli_near(100), SpendKind::Transfer)),
            Ok(())
        );
        let err = decide(
            &p,
            &daily(),
            NOON_DAY_0,
            &req(
                milli_near(100),
                SpendKind::FunctionCall {
                    method: "api_charge".into(),
                },
            ),
        )
        .unwrap_err();
        assert_eq!(err.code(), "MethodNotAllowed");
    }

    #[test]
    fn unauthorized_agent_rejected() {
        let mut r = req(milli_near(100), SpendKind::Transfer);
        r.caller = "stranger.testnet";
        let err = decide(&policy(), &daily(), NOON_DAY_0, &r).unwrap_err();
        assert_eq!(err.code(), "UnauthorizedAgent");
    }

    #[test]
    fn paused_rejects_even_valid_spend() {
        let mut p = policy();
        p.paused = true;
        let err = decide(&p, &daily(), NOON_DAY_0, &req(milli_near(100), SpendKind::Transfer))
            .unwrap_err();
        assert_eq!(err.code(), "Paused");
    }

    #[test]
    fn zero_amount_rejected() {
        let err = decide(&policy(), &daily(), NOON_DAY_0, &req(0, SpendKind::Transfer)).unwrap_err();
        assert_eq!(err.code(), "AmountZero");
    }

    #[test]
    fn zero_limits_are_fail_closed() {
        let p = Policy::fail_closed("agent.testnet");
        let err = decide(&p, &daily(), NOON_DAY_0, &req(1, SpendKind::Transfer)).unwrap_err();
        // Fresh policy: empty allowlist is deny-all, reported first.
        assert_eq!(err.code(), "ReceiverNotAllowed");
        // With a receiver filled in but limits still at 0, the cap rejects.
        let mut p = p;
        p.allowed_receivers = vec!["merchant.testnet".into()];
        let err = decide(&p, &daily(), NOON_DAY_0, &req(1, SpendKind::Transfer)).unwrap_err();
        assert_eq!(err.code(), "PerTxLimitExceeded");
    }

    #[test]
    fn insufficient_vault() {
        let mut r = req(near(1), SpendKind::Transfer);
        r.vault_available = milli_near(500);
        let err = decide(&policy(), &daily(), NOON_DAY_0, &r).unwrap_err();
        assert_eq!(err.code(), "InsufficientVault");
    }

    #[test]
    fn simulate_returns_structured_decision() {
        let ok = simulate(
            &policy(),
            &daily(),
            NOON_DAY_0,
            &req(milli_near(400), SpendKind::Transfer),
        );
        assert!(ok.allowed);
        assert_eq!(ok.code, "Allowed");
        assert_eq!(ok.daily_remaining, near(2));

        let mut r = req(near(10), SpendKind::Transfer);
        r.receiver = "attacker.near";
        let no = simulate(&policy(), &daily(), NOON_DAY_0, &r);
        assert!(!no.allowed);
        assert_eq!(no.code, "ReceiverNotAllowed");
    }

    #[test]
    fn prompt_injection_cannot_bypass_allowlist() {
        // The "agent" was told to ignore the policy and pay a stranger.
        // The engine never sees the prompt: it only sees caller, receiver, amount.
        let mut r = req(near(10), SpendKind::Transfer);
        r.caller = "agent.testnet";
        r.receiver = "evil-payout.near";
        let err = decide(&policy(), &daily(), NOON_DAY_0, &r).unwrap_err();
        assert_eq!(err.code(), "ReceiverNotAllowed");
        assert!(matches!(
            decide(&policy(), &daily(), NOON_DAY_0, &req(near(10), SpendKind::Transfer)),
            Err(DenyReason::PerTxLimitExceeded { .. })
        ));
    }

    #[test]
    fn parse_and_fmt_near_roundtrip() {
        assert_eq!(parse_near("1").unwrap(), near(1));
        assert_eq!(parse_near("1N").unwrap(), near(1));
        assert_eq!(parse_near("1.5 NEAR").unwrap(), milli_near(1500));
        assert_eq!(parse_near("0.4").unwrap(), milli_near(400));
        assert_eq!(parse_near("0").unwrap(), 0);
        assert_eq!(fmt_near(near(1)), "1 NEAR");
        assert_eq!(fmt_near(milli_near(400)), "0.4 NEAR");
        assert_eq!(fmt_near(milli_near(1500)), "1.5 NEAR");
        // raw yocto
        assert_eq!(
            parse_near("1000000000000000000000000").unwrap(),
            near(1)
        );
    }

    #[test]
    fn day_index_aligns_to_utc() {
        assert_eq!(day_index(0), 0);
        assert_eq!(day_index(DAY_NS - 1), 0);
        assert_eq!(day_index(DAY_NS), 1);
    }
}
