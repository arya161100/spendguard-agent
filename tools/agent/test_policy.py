"""Sanity tests for the Python policy mirror and the agent loop."""
import unittest
import sys
import os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from policy import (
    DailyWindow, Policy, SpendRequest, decide, simulate,
    milli_near, near, parse_near, fmt_near, DAY_NS,
)
from vault import demo_vault
from agent import INJECTED_TASK, LEGIT_TASK, run_demo, run_task


def req(amount, receiver="merchant.testnet", caller="agent.testnet", method=None):
    return SpendRequest(caller, receiver, amount, method, near(10))


class PolicyTests(unittest.TestCase):
    def setUp(self):
        self.p = Policy(
            agent="agent.testnet",
            per_tx_max=near(1),
            daily_max=near(2),
            allowed_receivers=["merchant.testnet"],
            allowed_methods=["api_charge"],
        )
        self.d = DailyWindow()
        self.noon = DAY_NS // 2

    def test_under_limit(self):
        self.assertIsNone(decide(self.p, self.d, self.noon, req(milli_near(400))))

    def test_over_per_tx(self):
        code, _ = decide(self.p, self.d, self.noon, req(milli_near(1500)))
        self.assertEqual(code, "PerTxLimitExceeded")

    def test_receiver(self):
        code, _ = decide(self.p, self.d, self.noon, req(milli_near(100), receiver="evil-payout.near"))
        self.assertEqual(code, "ReceiverNotAllowed")

    def test_injection_order(self):
        # 10 NEAR to a stranger is ReceiverNotAllowed, not PerTx, because
        # allowlist is checked first — same as the Rust engine.
        d = simulate(self.p, self.d, self.noon, req(near(10), receiver="evil-payout.near"))
        self.assertFalse(d.allowed)
        self.assertEqual(d.code, "ReceiverNotAllowed")

    def test_parse_fmt(self):
        self.assertEqual(parse_near("0.4"), milli_near(400))
        self.assertEqual(fmt_near(milli_near(400)), "0.4 NEAR")


class AgentTests(unittest.TestCase):
    def test_offline_demo_approves_then_rejects(self):
        demo = run_demo(prefer_llm=False)
        clean = demo["clean"]["results"]
        self.assertTrue(any(r["verdict"] == "APPROVE" for r in clean), clean)
        attack = demo["attack"]["results"]
        # At least one REJECT on the injected drain.
        self.assertTrue(any(r["verdict"] == "REJECT" for r in attack), attack)
        codes = [r["decision"]["code"] for r in attack if r["verdict"] == "REJECT"]
        self.assertIn("ReceiverNotAllowed", codes)
        # Vault only paid approved amounts (0.4 + possibly another 0.4 from the
        # injected task's legitimate half). Started at 10.
        from policy import parse_near
        bal = parse_near(demo["vault"]["balance_near"])
        self.assertLess(bal, parse_near("10"))
        self.assertGreater(bal, parse_near("9"))

    def test_legit_only(self):
        from vault import demo_vault
        v = demo_vault()
        out = run_task(v, LEGIT_TASK, prefer_llm=False)
        self.assertEqual(out["results"][0]["verdict"], "APPROVE")


if __name__ == "__main__":
    unittest.main()
