import unittest

from analyze import compare


class AnalyzeTests(unittest.TestCase):
    def test_simple_shared_pass(self):
        records = [
            {"agent": "a", "task": "x", "completed_at": 1, "reward": 1, "exception": False, "seconds": 2, "input_tokens": 10, "cached_input_tokens": 5, "output_tokens": 1},
            {"agent": "b", "task": "x", "completed_at": 1, "reward": 1, "exception": False, "seconds": 3, "input_tokens": 10, "cached_input_tokens": 5, "output_tokens": 1},
        ]
        result = compare(records, "a", "b")
        self.assertEqual(result["shared_passes"], ["x"])
        self.assertEqual(result["faster"]["a"], ["x"])


if __name__ == "__main__":
    unittest.main()
