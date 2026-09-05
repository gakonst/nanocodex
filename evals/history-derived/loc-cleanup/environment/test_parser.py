import unittest

from parser_api import parse_records


class ParserTests(unittest.TestCase):
    def test_simple_records(self):
        self.assertEqual(parse_records(["alpha, 1\n"]), [{"name": "alpha", "value": 1}])


if __name__ == "__main__":
    unittest.main()
