from __future__ import annotations

import json
import threading
import time
import unittest

from nanocodex import Nanocodex, SessionSnapshot, TurnResult
from support import MockResponsesServer, RequestRecord, user_texts
from support.websocket import WebSocketConnection


class OwnedLifecycleTests(unittest.TestCase):
    def test_prompt_acceptance_result_and_events_are_independent(self) -> None:
        generation_started = threading.Event()
        release_generation = threading.Event()

        def handle(
            server: MockResponsesServer,
            connection: WebSocketConnection,
            record: RequestRecord,
        ) -> None:
            if record.body.get("generate") is False:
                server.respond_warmup(connection, "resp-warmup")
                return
            generation_started.set()
            self.assertTrue(release_generation.wait(5))
            server.respond_final(connection, "accepted then completed", "resp-final")

        with MockResponsesServer(handle) as server:
            agent, events = Nanocodex(
                "test-key",
                thinking="none",
                instructions="Return the exact mock response.",
                websocket_url=server.endpoint,
            )
            started = time.perf_counter()
            turn = agent.prompt("prove acceptance is separate")
            acceptance_seconds = time.perf_counter() - started
            self.assertLess(acceptance_seconds, 0.1)
            self.assertTrue(generation_started.wait(5))

            outcome: list[TurnResult] = []
            waiter = threading.Thread(target=lambda: outcome.append(turn.result()))
            waiter.start()
            time.sleep(0.05)
            self.assertTrue(
                waiter.is_alive(), "result() returned before the model completed"
            )
            release_generation.set()
            waiter.join(5)

            self.assertEqual(outcome[0].final_message, "accepted then completed")
            observed = []
            while True:
                event = events.recv()
                self.assertIsNotNone(event)
                observed.append(event)
                if event.kind in {"run.completed", "run.failed"}:
                    break
            self.assertEqual(events.request_id, agent.session_id)
            self.assertEqual(observed[-1].kind, "run.completed")
            self.assertEqual(observed[-1].protocol_version, 1)
            self.assertEqual(observed[-1].request_id, agent.session_id)
            self.assertIsInstance(observed[-1].payload, dict)
            self.assertEqual(
                observed[-1].payload,
                json.loads(observed[-1].payload_json),
            )
            self.assertEqual(
                [event.seq for event in observed],
                sorted(event.seq for event in observed),
            )
            agent.shutdown()
            while events.recv() is not None:
                pass

    def test_follow_ons_reuse_one_socket_response_chain_and_cache_key(self) -> None:
        with MockResponsesServer() as server:
            agent, _ = Nanocodex(
                "test-key",
                thinking="none",
                prompt_cache_key="python-stable-cache",
                websocket_url=server.endpoint,
            )
            first = agent.prompt("first prompt").result()
            second = agent.prompt("second prompt").result()
            self.assertEqual(first.final_message, "first prompt")
            self.assertEqual(second.final_message, "second prompt")

            requests = server.wait_for_requests(3)
            warmup, initial, follow_on = [record.body for record in requests]
            self.assertEqual(server.connection_count, 1)
            self.assertFalse(warmup["generate"])
            self.assertEqual(initial["previous_response_id"], "resp-warmup-1")
            self.assertEqual(follow_on["previous_response_id"], "resp-final-2")
            self.assertEqual(initial["prompt_cache_key"], "python-stable-cache")
            self.assertEqual(follow_on["prompt_cache_key"], "python-stable-cache")
            self.assertEqual(user_texts(initial)[-1], "first prompt")
            self.assertEqual(user_texts(follow_on), ["second prompt"])
            agent.shutdown()

    def test_replacement_socket_replays_authoritative_history(self) -> None:
        def handle(
            server: MockResponsesServer,
            connection: WebSocketConnection,
            record: RequestRecord,
        ) -> None:
            if record.body.get("generate") is False:
                server.respond_warmup(connection, "resp-reconnect-warmup")
            elif record.connection_id == 1:
                server.respond_final(
                    connection,
                    "first answer",
                    "resp-before-reconnect",
                )
                connection.close()
            else:
                server.respond_final(
                    connection,
                    "second answer",
                    "resp-after-reconnect",
                )

        with MockResponsesServer(handle) as server:
            agent, _ = Nanocodex(
                "test-key",
                thinking="none",
                prompt_cache_key="python-reconnect-cache",
                websocket_url=server.endpoint,
            )
            self.assertEqual(
                agent.prompt("first reconnect prompt").result().final_message,
                "first answer",
            )
            self.assertEqual(
                agent.prompt("second reconnect prompt").result().final_message,
                "second answer",
            )
            records = server.wait_for_requests(3)
            replay = records[-1].body
            self.assertEqual(server.connection_count, 2)
            self.assertNotIn("previous_response_id", replay)
            self.assertEqual(
                replay["prompt_cache_key"],
                "python-reconnect-cache",
            )
            self.assertIn("first reconnect prompt", user_texts(replay))
            self.assertIn("second reconnect prompt", user_texts(replay))
            self.assertIn("first answer", json.dumps(replay))
            agent.shutdown()

    def test_lifecycle_branch_snapshot_resume_usage_and_cost(self) -> None:
        with MockResponsesServer() as server:
            agent, _ = Nanocodex(
                "test-key",
                thinking="none",
                instructions="Preserve exact Python test identifiers.",
                websocket_url=server.endpoint,
            )
            completed = agent.prompt("root checkpoint").result()
            self.assertEqual(completed.usage()["input_tokens"], 10)
            self.assertEqual(completed.usage()["cached_input_tokens"], 5)
            self.assertEqual(
                completed.usage()["estimated_cost"]["usd"],
                "0.0000875",
            )
            self.assertEqual(
                completed.usage()["cost_status"],
                "estimated_from_usage",
            )

            snapshot = completed.snapshot()
            self.assertIsInstance(snapshot, SessionSnapshot)
            encoded = snapshot.to_json()
            restored = SessionSnapshot.from_json(encoded)
            self.assertEqual(restored.version, 1)
            self.assertEqual(restored.workspace, snapshot.workspace)

            branch, _ = agent.fork_from(completed)
            self.assertEqual(
                branch.prompt("historical branch").result().final_message,
                "historical branch",
            )
            latest, _ = agent.fork()
            self.assertEqual(
                latest.prompt("latest branch").result().final_message,
                "latest branch",
            )
            sibling, _ = agent.spawn()
            self.assertEqual(
                sibling.prompt("clean sibling").result().final_message,
                "clean sibling",
            )
            agent.shutdown()

            resumed, _ = Nanocodex(
                "test-key",
                thinking="none",
                instructions="Preserve exact Python test identifiers.",
                resume=restored,
                websocket_url=server.endpoint,
            )
            self.assertEqual(
                resumed.prompt("resumed branch").result().final_message,
                "resumed branch",
            )
            for child in (branch, latest, sibling, resumed):
                child.shutdown()

            records = server.wait_for_requests(7)
            replay_requests = [
                record.body
                for record in records
                if any(
                    prompt in user_texts(record.body)
                    for prompt in (
                        "historical branch",
                        "latest branch",
                        "resumed branch",
                    )
                )
            ]
            self.assertEqual(len(replay_requests), 3)
            requests_by_prompt = {
                prompt: next(
                    request
                    for request in replay_requests
                    if prompt in user_texts(request)
                )
                for prompt in (
                    "historical branch",
                    "latest branch",
                    "resumed branch",
                )
            }
            for prompt in ("historical branch", "latest branch"):
                request = requests_by_prompt[prompt]
                self.assertEqual(request["previous_response_id"], "resp-final-2")
                self.assertEqual(user_texts(request), [prompt])

            resumed_request = requests_by_prompt["resumed branch"]
            self.assertNotIn("previous_response_id", resumed_request)
            self.assertIn("root checkpoint", user_texts(resumed_request))

    def test_steer_cancel_and_compact_cross_the_boundary(self) -> None:
        initial_started = threading.Event()
        release_initial = threading.Event()

        def handle(
            server: MockResponsesServer,
            connection: WebSocketConnection,
            record: RequestRecord,
        ) -> None:
            if record.body.get("generate") is False:
                server.respond_warmup(connection, "resp-warmup")
            elif record.body.get("input", [])[-1:] == [{"type": "compaction_trigger"}]:
                server.respond_compaction(connection, "resp-compact")
            elif user_texts(record.body)[-1:] == ["initial task"]:
                initial_started.set()
                self.assertTrue(release_initial.wait(5))
                server.respond_final(connection, "first boundary", "resp-first")
            elif user_texts(record.body)[-1:] == ["steered constraint"]:
                server.respond_final(connection, "steered result", "resp-steered")
            elif user_texts(record.body)[-1:] == ["cancel this turn"]:
                initial_started.set()
                while connection.recv_json() is not None:
                    pass
            else:
                server.respond_final(connection)

        with MockResponsesServer(handle) as server:
            agent, _ = Nanocodex(
                "test-key",
                thinking="none",
                websocket_url=server.endpoint,
            )
            turn = agent.prompt("initial task")
            self.assertTrue(initial_started.wait(5))
            turn.steer("steered constraint")
            release_initial.set()
            self.assertEqual(turn.result().final_message, "steered result")
            with self.assertRaisesRegex(RuntimeError, "already complete"):
                turn.steer("too late")

            agent.compact()
            post_compaction = agent.prompt("after compaction").result()
            self.assertEqual(post_compaction.final_message, "done")
            requests = server.wait_for_requests(5)
            compact = next(
                record.body
                for record in requests
                if record.body.get("input", [])[-1:] == [{"type": "compaction_trigger"}]
            )
            self.assertEqual(compact["previous_response_id"], "resp-steered")

            initial_started.clear()
            cancelled = agent.prompt("cancel this turn")
            self.assertTrue(initial_started.wait(5))
            cancelled.cancel()
            with self.assertRaisesRegex(RuntimeError, "cancel"):
                cancelled.result()
            agent.shutdown()
            with self.assertRaisesRegex(RuntimeError, "shut down"):
                agent.prompt("must not be accepted")

    def test_shutdown_cancels_active_work_and_closes_the_event_stream(self) -> None:
        generation_started = threading.Event()

        def handle(
            server: MockResponsesServer,
            connection: WebSocketConnection,
            record: RequestRecord,
        ) -> None:
            if record.body.get("generate") is False:
                server.respond_warmup(connection, "resp-shutdown-warmup")
                return
            generation_started.set()
            while connection.recv_json() is not None:
                pass

        with MockResponsesServer(handle) as server:
            agent, events = Nanocodex(
                "test-key",
                thinking="none",
                websocket_url=server.endpoint,
            )
            turn = agent.prompt("wait for explicit shutdown")
            self.assertTrue(generation_started.wait(5))
            agent.shutdown()
            with self.assertRaisesRegex(RuntimeError, "cancel"):
                turn.result()

            terminal = None
            while event := events.recv():
                if event.kind in {"run.completed", "run.failed"}:
                    terminal = event
            self.assertIsNotNone(terminal)
            self.assertEqual(terminal.kind, "run.failed")
            self.assertEqual(terminal.payload["status"], "cancelled")


if __name__ == "__main__":
    unittest.main()
