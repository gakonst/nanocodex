import unittest

from client import fetch, format_result


class ClientTests(unittest.TestCase):
    def test_success_and_format(self):
        self.assertEqual(fetch(lambda: 7, lambda _delay: None), 7)
        self.assertEqual(format_result(7), "value=7")


if __name__ == "__main__":
    unittest.main()
