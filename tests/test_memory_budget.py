"""No network or credential: verify the explicit paid-evaluation budget guard."""
import concurrent.futures
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('relay', Path(__file__).resolve().parents[1]/'scripts/memory-deepseek-relay.py')
relay = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(relay)


class BudgetTests(unittest.TestCase):
    def test_resume_keeps_spending_and_charges_unfinished_requests(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'ledger.json'
            budget = relay.Budget(5, path)
            body = {'messages':[{'content':'模拟'}], 'max_tokens':8000}
            amount = budget.reserve(body)
            budget.finish(amount, 200, {'prompt_tokens':1000,'completion_tokens':100}, 0)
            pending = budget.reserve(body)
            resumed = relay.Budget(5, path, resume=True)
            self.assertAlmostEqual(resumed.spent, budget.spent + pending)
            self.assertEqual(resumed.reserved, 0)
            self.assertEqual(len(resumed.rows), 2)
            resumed.save()
            self.assertAlmostEqual(relay.Budget(5, path, resume=True).spent, resumed.spent)
            with self.assertRaises(ValueError):
                relay.Budget(4, path, resume=True)

    def test_concurrent_reservations_cannot_overspend_and_unknown_usage_is_retained(self):
        with tempfile.TemporaryDirectory() as directory:
            budget = relay.Budget(.1, Path(directory)/'ledger.json')
            body = {'messages': [{'role':'user','content':'独立模拟资料'}], 'max_tokens':2500}
            with concurrent.futures.ThreadPoolExecutor(max_workers=20) as pool:
                reservations = list(pool.map(lambda _:budget.reserve(body), range(50)))
            accepted = [amount for amount in reservations if amount is not None]
            self.assertGreater(len(accepted), 0)
            self.assertLess(len(accepted), 50)
            self.assertLessEqual(sum(accepted), .1)
            for amount in accepted:
                budget.finish(amount, 502, None, 0)
            self.assertAlmostEqual(budget.spent, sum(accepted))
            self.assertAlmostEqual(budget.reserved, 0)
            self.assertIsNone(budget.reserve(body))

    def test_known_usage_releases_unused_reservation_and_counts_cached_input_conservatively(self):
        with tempfile.TemporaryDirectory() as directory:
            budget = relay.Budget(5, Path(directory)/'ledger.json')
            amount = budget.reserve({'messages':[{'content':'模拟'}],'max_tokens':8000})
            budget.finish(amount, 200, {'prompt_tokens':1000,'completion_tokens':100,'prompt_cache_hit_tokens':1000}, 0)
            self.assertAlmostEqual(budget.spent, .0028)
            self.assertAlmostEqual(budget.reserved, 0)
            self.assertLess(budget.spent, amount)


if __name__ == '__main__':
    unittest.main()
