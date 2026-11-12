"""In-memory / on-disk vault used by the local agent and the web UI."""

from __future__ import annotations

import json
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Optional

from policy import (
    DEFAULT_RESERVE,
    DailyWindow,
    Decision,
    Policy,
    SpendRequest,
    YOCTO,
    day_index,
    fmt_near,
    parse_near,
    simulate,
)


def _now_ns() -> int:
    return time.time_ns()


@dataclass
class LogEntry:
    ts_ns: int
    event: str  # policy_set, spend_approved, spend_denied, agent_thought, ...
    message: str
    allowed: Optional[bool] = None
    code: str = ""
    caller: str = ""
    receiver: str = ""
    amount_near: str = ""
    method: str = ""
    memo: str = ""
    thought: str = ""
    source: str = ""  # "human", "agent-llm", "agent-fallback", "demo"

    def as_dict(self) -> dict:
        return asdict(self)


@dataclass
class Vault:
    owner: str = "owner.testnet"
    policy: Policy = field(default_factory=Policy)
    daily: DailyWindow = field(default_factory=DailyWindow)
    balance: int = 10 * YOCTO
    storage_reserve: int = DEFAULT_RESERVE
    log: list[LogEntry] = field(default_factory=list)

    def available(self) -> int:
        return max(0, self.balance - self.storage_reserve)

    def request(self, caller: str, receiver: str, amount: int, method: Optional[str] = None) -> SpendRequest:
        return SpendRequest(caller, receiver, amount, method, self.available())

    def simulate(
        self,
        caller: str,
        receiver: str,
        amount: int,
        method: Optional[str] = None,
        now_ns: Optional[int] = None,
    ) -> Decision:
        now = _now_ns() if now_ns is None else now_ns
        return simulate(self.policy, self.daily, now, self.request(caller, receiver, amount, method))

    def spend(
        self,
        caller: str,
        receiver: str,
        amount: int,
        method: Optional[str] = None,
        memo: str = "",
        thought: str = "",
        source: str = "human",
        now_ns: Optional[int] = None,
    ) -> Decision:
        now = _now_ns() if now_ns is None else now_ns
        decision = self.simulate(caller, receiver, amount, method, now_ns=now)
        kind = f"call:{method}" if method else "transfer"
        if decision.allowed:
            self.daily.record_success(now, amount)
            self.balance -= amount
            self.log.append(
                LogEntry(
                    ts_ns=now,
                    event="spend_approved",
                    message=f"SUCCESS {fmt_near(amount)} → {receiver} ({kind})"
                    + (f" memo={memo!r}" if memo else ""),
                    allowed=True,
                    code="Allowed",
                    caller=caller,
                    receiver=receiver,
                    amount_near=fmt_near(amount),
                    method=method or "",
                    memo=memo,
                    thought=thought,
                    source=source,
                )
            )
        else:
            self.log.append(
                LogEntry(
                    ts_ns=now,
                    event="spend_denied",
                    message=f"REJECTED {fmt_near(amount)} → {receiver}: {decision.reason}",
                    allowed=False,
                    code=decision.code,
                    caller=caller,
                    receiver=receiver,
                    amount_near=fmt_near(amount),
                    method=method or "",
                    memo=memo,
                    thought=thought,
                    source=source,
                )
            )
        return decision

    def set_policy(
        self,
        per_tx,
        daily,
        receivers: list[str],
        methods: list[str],
        agent: Optional[str] = None,
    ) -> None:
        self.policy.per_tx_max = parse_near(str(per_tx)) if not isinstance(per_tx, int) else per_tx
        self.policy.daily_max = parse_near(str(daily)) if not isinstance(daily, int) else daily
        if self.policy.per_tx_max > self.policy.daily_max:
            raise ValueError("per-tx max cannot exceed daily max")
        self.policy.allowed_receivers = list(receivers)
        self.policy.allowed_methods = list(methods)
        if agent:
            self.policy.agent = agent
        self.log.append(
            LogEntry(
                ts_ns=_now_ns(),
                event="policy_set",
                message=(
                    f"policy: per-tx {fmt_near(self.policy.per_tx_max)}, "
                    f"daily {fmt_near(self.policy.daily_max)}, "
                    f"receivers={self.policy.allowed_receivers}, "
                    f"methods={self.policy.allowed_methods}"
                ),
                source="human",
            )
        )

    def snapshot(self, now_ns: Optional[int] = None) -> dict:
        now = _now_ns() if now_ns is None else now_ns
        spent = self.daily.spent_at(now)
        return {
            "owner": self.owner,
            "agent": self.policy.agent,
            "paused": self.policy.paused,
            "balance": str(self.balance),
            "balance_near": fmt_near(self.balance),
            "available": str(self.available()),
            "available_near": fmt_near(self.available()),
            "storage_reserve_near": fmt_near(self.storage_reserve),
            "per_tx_max": str(self.policy.per_tx_max),
            "per_tx_max_near": fmt_near(self.policy.per_tx_max),
            "daily_max": str(self.policy.daily_max),
            "daily_max_near": fmt_near(self.policy.daily_max),
            "daily_spent": str(spent),
            "daily_spent_near": fmt_near(spent),
            "daily_remaining_near": fmt_near(self.daily.remaining(self.policy.daily_max, now)),
            "day_index": day_index(now),
            "allowed_receivers": list(self.policy.allowed_receivers),
            "allowed_methods": list(self.policy.allowed_methods),
            "log": [e.as_dict() for e in self.log],
        }


def demo_vault() -> Vault:
    """A vault already configured with the documented demo policy and 10 NEAR."""
    v = Vault()
    v.owner = "owner.testnet"
    v.policy.agent = "agent.testnet"
    v.set_policy("1", "2", ["merchant.testnet"], ["api_charge"])
    return v
