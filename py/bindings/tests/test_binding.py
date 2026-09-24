import os
import unittest

from nanocodex import (
    Nanocodex,
    SessionSnapshot,
    TurnResult,
)


def drain(events: object) -> None:
    while events.recv_json() is not None:
        pass


class BindingTests(unittest.TestCase):
    def test_constructs_owned_handle_and_event_stream_without_exposing_secret(
        self,
    ) -> None:
        secret = "private-test-value"
        agent, events = Nanocodex(
            secret,
            model="gpt-6-sol",
            thinking="medium",
            reasoning_mode="standard",
        )
        self.assertNotIn(secret, repr(agent))
        self.assertEqual(events.request_id, agent.session_id)
        agent.shutdown()
        drain(events)

    def test_configuration_errors_cross_the_boundary(self) -> None:
        with self.assertRaisesRegex(ValueError, "expected none"):
            Nanocodex("test-key", thinking="impossible")

        agent, _ = Nanocodex("test-key")
        with self.assertRaisesRegex(ValueError, "expected none"):
            agent.set_thinking("impossible")
        agent.shutdown()

        with self.assertRaisesRegex(ValueError, "OpenAI credentials are empty"):
            Nanocodex("")

        with self.assertRaises(ValueError):
            Nanocodex("test-key", session_id="not-a-uuid-v7")

    def test_fork_before_safe_boundary_is_typed(self) -> None:
        agent, events = Nanocodex("test-key", thinking="low")
        with self.assertRaises(RuntimeError):
            agent.fork()
        agent.shutdown()
        drain(events)

    def test_empty_steer_is_rejected(self) -> None:
        agent, events = Nanocodex("test-key", thinking="low")
        turn = agent.prompt("queued for steer rejection")
        with self.assertRaisesRegex(
            RuntimeError, "steer instruction must not be empty"
        ):
            turn.steer("")
        turn.cancel()
        with self.assertRaises(RuntimeError):
            turn.result()
        agent.shutdown()
        drain(events)

    def test_fork_from_requires_a_typed_result(self) -> None:
        agent, events = Nanocodex("test-key", thinking="low")
        turn = agent.prompt("incomplete")
        with self.assertRaises(TypeError):
            agent.fork_from(turn)
        turn.cancel()
        with self.assertRaises(RuntimeError):
            turn.result()
        agent.shutdown()
        drain(events)

    def test_snapshot_rejects_invalid_json(self) -> None:
        with self.assertRaises(ValueError):
            SessionSnapshot.from_json('{"version": 1}')

    @unittest.skipUnless(
        os.environ.get("OPENAI_API_KEY"), "live API key not configured"
    )
    def test_live_follow_on_prompting(self) -> None:
        agent, events = Nanocodex(os.environ["OPENAI_API_KEY"], thinking="low")
        first = agent.prompt("Remember the token PYO3_LIVE. Reply with OK.")
        first_result = first.result()
        self.assertIsInstance(first_result, TurnResult)
        self.assertIn("OK", first_result.final_message)
        second = agent.prompt(
            "What token did I ask you to remember? Reply with only it."
        )
        self.assertEqual(second.result().final_message.strip(), "PYO3_LIVE")
        agent.shutdown()
        drain(events)


if __name__ == "__main__":
    unittest.main()
