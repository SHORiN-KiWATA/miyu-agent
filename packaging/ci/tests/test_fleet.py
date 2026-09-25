"""走查红绿账（testkit/fleet.py）的判定与对账规则（09-25）。"""

import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location('fleet', ROOT/'testkit'/'fleet.py')
fleet = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fleet)


class VerdictTests(unittest.TestCase):
    def test_a_nonzero_exit_is_red(self):
        self.assertFalse(fleet.verdict(1, '9/9 passed', {}))

    def test_a_short_count_is_red_even_when_the_script_exits_zero(self):
        self.assertFalse(fleet.verdict(0, '12/13 passed', {}))
        self.assertTrue(fleet.verdict(0, '13/13 passed', {}))

    def test_a_required_verdict_line_must_be_printed(self):
        entry = {'pass': '(?m)^通过$'}
        self.assertTrue(fleet.verdict(0, '{}\n通过\n产物：x', entry))
        self.assertFalse(fleet.verdict(0, '{}\n红: [\'a\']\n产物：x', entry))
        # 什么都没打印就退出 0 的，照样不算绿。
        self.assertFalse(fleet.verdict(0, '', {'pass': '(\\d+)/\\1 passed'}))


class LedgerTests(unittest.TestCase):
    def ledger(self, reds):
        return {
            'walkthroughs': [{'name': 'a', 'argv': ['python3', 'testkit/fleet.py']},
                             {'name': 'b', 'argv': ['python3', 'testkit/fleet.py']}],
            'known_red': [{'name': name, 'reason': 'why'} for name in reds],
        }

    def test_the_committed_ledger_is_well_formed(self):
        self.assertEqual(fleet.check(fleet.load()), [])

    def test_known_reds_only_shrink(self):
        self.assertEqual(fleet.check(self.ledger(['a']), self.ledger(['a', 'b'])), [])
        problems = fleet.check(self.ledger(['a', 'b']), self.ledger(['a']))
        self.assertEqual(len(problems), 1)
        self.assertIn('b', problems[0])

    def test_a_known_red_needs_a_reason_and_a_registered_walkthrough(self):
        ledger = self.ledger([])
        ledger['known_red'] = [{'name': 'a', 'reason': ' '}, {'name': 'zzz', 'reason': 'x'}]
        problems = fleet.check(ledger)
        self.assertEqual(len(problems), 2)


if __name__ == '__main__':
    unittest.main()
