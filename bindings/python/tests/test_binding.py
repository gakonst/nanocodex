import os
import unittest

from nanocodex import Nanocodex


class BindingTests(unittest.TestCase):
    def test_constructs_owned_handle_and_event_stream_without_exposing_secret(self) -> None:
        secret = "private-test-value"
        agent, events = Nanocodex(
            secret, thinking="none", reasoning_mode="pro"
        )
        self.assertNotIn(secret, repr(agent))
        self.assertTrue(callable(agent.prompt))
        self.assertTrue(callable(events.recv_json))

    def test_configuration_errors_cross_the_boundary(self) -> None:
        with self.assertRaisesRegex(ValueError, "expected none"):
            Nanocodex("test-key", thinking="impossible")

        with self.assertRaisesRegex(ValueError, "expected standard or pro"):
            Nanocodex("test-key", reasoning_mode="impossible")

        with self.assertRaisesRegex(RuntimeError, "OpenAI credentials are empty"):
            Nanocodex("")

    @unittest.skipUnless(os.environ.get("OPENAI_API_KEY"), "live API key not configured")
    def test_live_follow_on_prompting(self) -> None:
        agent, _ = Nanocodex(os.environ["OPENAI_API_KEY"], thinking="low")
        first = agent.prompt("Remember the token PYO3_LIVE. Reply with OK.")
        self.assertIn("OK", first.result())
        second = agent.prompt("What token did I ask you to remember? Reply with only it.")
        self.assertEqual(second.result().strip(), "PYO3_LIVE")


if __name__ == "__main__":
    unittest.main()
