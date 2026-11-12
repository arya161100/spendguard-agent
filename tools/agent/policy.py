"""SpendGuard policy engine (Python).

Mirrors crates/spendguard-policy: the same check order, the same fail-closed
defaults, the same reason codes. The Rust crate remains authoritative for the
on-chain contract; this copy lets the local agent and the web UI run with
nothing but Python 3. Keep the two in sync if you change a rule.
"""

from __future__ import annotations

from dataclasses import dataclass, field, asdict
from typing import Optional

YOCTO = 10**24
DAY_NS = 86_400_000_000_000
MAX_ALLOWLIST = 64
DEFAULT_RESERVE = YOCTO // 20  # 0.05 NEAR


def near(n: int) -> int:
    return n * YOCTO


def milli_near(n: int) -> int:
    return n * YOCTO // 1000


def day_index(ts_ns: int) -> int:
    return ts_ns // DAY_NS


def fmt_near(yocto: int) -> str:
    if yocto == 0:
        return "0 NEAR"
    whole, rest = divmod(yocto, YOCTO)
    if rest == 0:
        return f"{whole} NEAR"
    frac = f"{rest:024d}"[:6].rstrip("0")
    return f"{whole}.{frac} NEAR" if frac else f"{whole} NEAR"


def parse_near(text: str) -> int:
    s = (
        text.strip()
        .removesuffix("NEAR")
        .removesuffix("near")
        .removesuffix("N")
        .removesuffix("n")
        .strip()
        .replace("_", "")
    )
    if not s:
        raise ValueError("empty amount")
    if s.isdigit() and len(s) >= 20:
        return int(s)
    if "." in s:
        whole, frac = s.split(".", 1)
        if not whole.isdigit() or not frac.isdigit() or not frac or len(frac) > 24:
            raise ValueError(f"invalid amount '{text}'")
        frac = frac.ljust(24, "0")
        return int(whole) * YOCTO + int(frac)
    if s.isdigit():
        return int(s) * YOCTO
    raise ValueError(f"invalid amount '{text}'")


@dataclass
class Policy:
    agent: str = "agent.testnet"
    per_tx_max: int = 0
    daily_max: int = 0
    allowed_receivers: list[str] = field(default_factory=list)
    allowed_methods: list[str] = field(default_factory=list)
    paused: bool = False


@dataclass
class DailyWindow:
    day_index: int = 0
    spent: int = 0

    def spent_at(self, now_ns: int) -> int:
        return self.spent if self.day_index == day_index(now_ns) else 0

    def remaining(self, daily_max: int, now_ns: int) -> int:
        return max(0, daily_max - self.spent_at(now_ns))

    def record_success(self, now_ns: int, amount: int) -> None:
        today = day_index(now_ns)
        if self.day_index != today:
            self.day_index = today
            self.spent = 0
        self.spent += amount


@dataclass
class SpendRequest:
    caller: str
    receiver: str
    amount: int
    method: Optional[str] = None  # None = native transfer
    vault_available: int = 0


@dataclass
class Decision:
    allowed: bool
    code: str
    reason: str
    amount: int
    per_tx_max: int
    daily_remaining: int
    daily_spent: int
    daily_max: int

    def as_dict(self) -> dict:
        d = asdict(self)
        for k in ("amount", "per_tx_max", "daily_remaining", "daily_spent", "daily_max"):
            d[k] = str(d[k])
            d[k + "_near"] = fmt_near(int(d[k]))
        return d


def _allowed(lst: list[str], item: str) -> bool:
    return bool(lst) and item in lst


def decide(policy: Policy, daily: DailyWindow, now_ns: int, req: SpendRequest):
    """Return (code, reason) if denied, or None if allowed."""
    if policy.paused:
        return "Paused", "Paused: the owner has paused the agent"
    if req.caller != policy.agent:
        return (
            "UnauthorizedAgent",
            f"UnauthorizedAgent: caller '{req.caller}' is not the authorized agent '{policy.agent}'",
        )
    if req.amount == 0:
        return "AmountZero", "AmountZero: spend amount must be greater than zero"
    if not _allowed(policy.allowed_receivers, req.receiver):
        return (
            "ReceiverNotAllowed",
            f"ReceiverNotAllowed: '{req.receiver}' is not on the owner allowlist",
        )
    if req.method is not None and not _allowed(policy.allowed_methods, req.method):
        return (
            "MethodNotAllowed",
            f"MethodNotAllowed: '{req.method}' is not on the owner allowlist",
        )
    if req.amount > policy.per_tx_max:
        return (
            "PerTxLimitExceeded",
            f"PerTxLimitExceeded: amount {fmt_near(req.amount)} exceeds per-tx max {fmt_near(policy.per_tx_max)}",
        )
    remaining = daily.remaining(policy.daily_max, now_ns)
    if req.amount > remaining:
        return (
            "DailyLimitExceeded",
            f"DailyLimitExceeded: amount {fmt_near(req.amount)} exceeds daily remaining {fmt_near(remaining)}",
        )
    if req.amount > req.vault_available:
        return (
            "InsufficientVault",
            f"InsufficientVault: need {fmt_near(req.amount)} but vault has {fmt_near(req.vault_available)} available (after storage reserve)",
        )
    return None


def simulate(policy: Policy, daily: DailyWindow, now_ns: int, req: SpendRequest) -> Decision:
    spent = daily.spent_at(now_ns)
    remaining = daily.remaining(policy.daily_max, now_ns)
    denied = decide(policy, daily, now_ns, req)
    if denied is None:
        return Decision(
            True, "Allowed", "Allowed: all policy checks passed",
            req.amount, policy.per_tx_max, remaining, spent, policy.daily_max,
        )
    code, reason = denied
    return Decision(
        False, code, reason,
        req.amount, policy.per_tx_max, remaining, spent, policy.daily_max,
    )
