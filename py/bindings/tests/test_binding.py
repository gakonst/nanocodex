import os
import unittest

from nanocodex import (
    AgentEvent,
    Nanocodex,
    SessionSnapshot,
    Turn,
    TurnResult,
    __version__,
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
            model="gpt-5.6-terra",
            thinking="none",
            reasoning_mode="pro",
        )
        self.assertNotIn(secret, repr(agent))
        self.assertTrue(callable(agent.prompt))
        self.assertTrue(callable(agent.spawn))
        self.assertTrue(callable(agent.fork))
        self.assertTrue(callable(agent.fork_from))
        self.assertTrue(callable(agent.shutdown))
        self.assertEqual(events.request_id, agent.session_id)
        agent.set_thinking("high")
        agent.set_fast_mode(True)
        self.assertTrue(callable(events.recv_json))
        self.assertRegex(__version__, r"^\d+\.\d+\.\d+")
        agent.shutdown()
        drain(events)

    def test_configuration_errors_cross_the_boundary(self) -> None:
        astra, astra_events = Nanocodex(
            "test-key", model="gpt-6-astra", thinking="low"
        )
        astra.shutdown()
        drain(astra_events)

        with self.assertRaisesRegex(ValueError, "GPT-6 Astra requires"):
            Nanocodex("test-key", model="gpt-6-astra", thinking="none")

        with self.assertRaisesRegex(ValueError, "does not support pro"):
            Nanocodex(
                "test-key", model="gpt-6-astra", reasoning_mode="pro"
            )

        with self.assertRaisesRegex(ValueError, "expected none"):
            Nanocodex("test-key", thinking="impossible")

        with self.assertRaisesRegex(ValueError, "expected standard or pro"):
            Nanocodex("test-key", reasoning_mode="impossible")

        with self.assertRaisesRegex(ValueError, "expected gpt-5.6-sol"):
            Nanocodex("test-key", model="impossible")

        agent, _ = Nanocodex("test-key")
        with self.assertRaisesRegex(ValueError, "expected none"):
            agent.set_thinking("impossible")
        agent.shutdown()

        with self.assertRaisesRegex(ValueError, "OpenAI credentials are empty"):
            Nanocodex("")

        with self.assertRaises(ValueError):
            Nanocodex("test-key", session_id="not-a-uuid-v7")

    def test_spawn_returns_independent_agent_without_network(self) -> None:
        agent, events = Nanocodex("test-key", thinking="none")
        child, child_events = agent.spawn()
        self.assertTrue(callable(child.prompt))
        self.assertTrue(callable(child_events.recv_json))
        self.assertIsNot(agent, child)
        self.assertNotEqual(agent.session_id, child.session_id)
        child.shutdown()
        agent.shutdown()
        drain(child_events)
        drain(events)

    def test_fork_before_safe_boundary_is_typed(self) -> None:
        agent, events = Nanocodex("test-key", thinking="none")
        with self.assertRaises(RuntimeError):
            agent.fork()
        agent.shutdown()
        drain(events)

    def test_empty_steer_is_rejected(self) -> None:
        agent, events = Nanocodex("test-key", thinking="none")
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
        agent, events = Nanocodex("test-key", thinking="none")
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

    def test_internal_transport_and_turn_ids_are_not_public_properties(self) -> None:
        for public_type in (Nanocodex, Turn, TurnResult, SessionSnapshot, AgentEvent):
            names = dir(public_type)
            self.assertNotIn("response_id", names)
            self.assertNotIn("previous_response_id", names)
            self.assertNotIn("turn_id", names)

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
