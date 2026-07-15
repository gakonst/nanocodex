"""Run the Rust harness locally while Harbor owns the task environment."""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import shlex
import signal
import sys
from pathlib import Path
from typing import Any
from uuid import uuid4

from harbor.agents.base import BaseAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext
from harbor.models.trajectories import Agent, FinalMetrics, Step, Trajectory
from harbor.utils.trajectory_utils import format_trajectory_json


PROTOCOL_VERSION = 1
TERMINAL_EVENT_TYPES = {"run.completed", "run.failed"}


class HarnessAgent(BaseAgent):
    """A Harbor external agent backed by a local JSONL child process."""

    SUPPORTS_ATIF = True
    _VERSION = "0.1.0"

    @staticmethod
    def name() -> str:
        return "harness"

    def version(self) -> str:
        return self._VERSION

    async def setup(self, environment: BaseEnvironment) -> None:
        """The external agent has nothing to install in the task container."""

    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        del environment, context
        self.logs_dir.mkdir(parents=True, exist_ok=True)

        request_id = str(self.context_id or self.session_id or uuid4())
        request = {
            "protocol_version": PROTOCOL_VERSION,
            "request_id": request_id,
            "seq": 1,
            "type": "task.start",
            "payload": {
                "instruction": instruction,
                "workspace": "/app",
                "metadata": {
                    "harbor_session_id": self.session_id,
                    "harbor_context_id": str(self.context_id)
                    if self.context_id
                    else None,
                },
            },
        }
        request_line = json.dumps(request, separators=(",", ":"))
        (self.logs_dir / "input.jsonl").write_text(
            f"{request_line}\n", encoding="utf-8"
        )

        command = shlex.split(
            os.environ.get("HARNESS_COMMAND", "cargo run --quiet -- run")
        )
        if not command:
            raise RuntimeError("HARNESS_COMMAND resolved to an empty command")

        process = await asyncio.create_subprocess_exec(
            *command,
            cwd=self._repository_root(),
            env={**os.environ, **self.extra_env},
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            start_new_session=True,
        )
        if process.stdin is None or process.stdout is None or process.stderr is None:
            await self._stop_process(process)
            raise RuntimeError("failed to open harness process pipes")

        stderr_task = asyncio.create_task(self._drain_stderr(process.stderr))
        saw_terminal = False
        expected_seq = 1
        try:
            process.stdin.write(f"{request_line}\n".encode())
            await process.stdin.drain()

            events_path = self.logs_dir / "events.jsonl"
            with events_path.open("w", encoding="utf-8") as events_file:
                while raw_line := await process.stdout.readline():
                    line = raw_line.decode("utf-8").rstrip("\n")
                    events_file.write(f"{line}\n")
                    events_file.flush()
                    print(line, flush=True)

                    event = self._validate_event(line, request_id, expected_seq)
                    expected_seq += 1
                    if saw_terminal:
                        raise RuntimeError("harness emitted an event after its terminal event")
                    if event["type"] == "tool.call":
                        raise RuntimeError(
                            "tool.call is not supported by the Phase 0 adapter"
                        )
                    if event["type"] in TERMINAL_EVENT_TYPES:
                        saw_terminal = True

            process.stdin.close()
            return_code = await process.wait()
            await stderr_task
            if return_code != 0:
                raise RuntimeError(f"harness process exited with status {return_code}")
            if not saw_terminal:
                raise RuntimeError("harness process exited without a terminal event")
        except asyncio.CancelledError:
            await self._send_cancel(process, request_id)
            await self._stop_process(process)
            raise
        except Exception:
            await self._stop_process(process)
            raise
        finally:
            if not stderr_task.done():
                stderr_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await stderr_task

    def populate_context_post_run(self, context: AgentContext) -> None:
        request = self._read_single_jsonl(self.logs_dir / "input.jsonl")
        events = self._read_jsonl(self.logs_dir / "events.jsonl")
        instruction = request["payload"]["instruction"]

        message = next(
            (
                event["payload"].get("text", "")
                for event in events
                if event.get("type") == "assistant.message"
            ),
            "Harness emitted no assistant message.",
        )
        terminal = next(
            (event for event in events if event.get("type") in TERMINAL_EVENT_TYPES),
            None,
        )
        incomplete = terminal is None
        terminal_type = terminal["type"] if terminal else None
        terminal_payload = terminal["payload"] if terminal else None

        trajectory = Trajectory(
            schema_version="ATIF-v1.7",
            session_id=request["request_id"],
            agent=Agent(
                name=self.name(),
                version=self.version(),
                extra={"protocol_version": PROTOCOL_VERSION},
            ),
            steps=[
                Step(step_id=1, source="user", message=instruction),
                Step(
                    step_id=2,
                    source="agent",
                    message=message,
                    llm_call_count=0,
                    extra={
                        "incomplete": incomplete,
                        "terminal_event_type": terminal_type,
                        "terminal_payload": terminal_payload,
                    },
                ),
            ],
            notes=(
                "The process ended before emitting a terminal event."
                if incomplete
                else None
            ),
            final_metrics=FinalMetrics(
                total_prompt_tokens=0,
                total_completion_tokens=0,
                total_cached_tokens=0,
                total_cost_usd=0.0,
                total_steps=2,
                extra={"model_calls": 0, "tool_calls": 0},
            ),
            extra={
                "input_jsonl": "input.jsonl",
                "events_jsonl": "events.jsonl",
                "stderr_log": "stderr.log",
                "incomplete": incomplete,
            },
        )
        (self.logs_dir / "trajectory.json").write_text(
            format_trajectory_json(trajectory.to_json_dict()), encoding="utf-8"
        )

        context.n_input_tokens = 0
        context.n_cache_tokens = 0
        context.n_output_tokens = 0
        context.cost_usd = 0.0
        context.metadata = {
            "protocol_version": PROTOCOL_VERSION,
            "terminal_event_type": terminal_type,
            "incomplete": incomplete,
        }

    async def _drain_stderr(self, stream: asyncio.StreamReader) -> None:
        path = self.logs_dir / "stderr.log"
        with path.open("w", encoding="utf-8") as stderr_file:
            while raw_line := await stream.readline():
                line = raw_line.decode("utf-8", errors="replace")
                stderr_file.write(line)
                stderr_file.flush()
                print(line, end="", file=sys.stderr, flush=True)

    @staticmethod
    def _validate_event(
        line: str, request_id: str, expected_seq: int
    ) -> dict[str, Any]:
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise RuntimeError(f"harness stdout was not JSONL: {error}") from error
        if not isinstance(event, dict):
            raise RuntimeError("harness stdout event must be a JSON object")
        if event.get("protocol_version") != PROTOCOL_VERSION:
            raise RuntimeError("harness stdout used an unsupported protocol version")
        if event.get("request_id") != request_id:
            raise RuntimeError("harness stdout request_id did not match its input")
        if event.get("seq") != expected_seq:
            raise RuntimeError(
                f"harness stdout event seq was {event.get('seq')!r}; "
                f"expected {expected_seq}"
            )
        if not isinstance(event.get("type"), str):
            raise RuntimeError("harness stdout event had no string type")
        if not isinstance(event.get("payload"), dict):
            raise RuntimeError("harness stdout event payload must be an object")
        return event

    async def _send_cancel(
        self, process: asyncio.subprocess.Process, request_id: str
    ) -> None:
        if process.returncode is not None or process.stdin is None:
            return
        cancel = {
            "protocol_version": PROTOCOL_VERSION,
            "request_id": request_id,
            "seq": 2,
            "type": "control.cancel",
            "payload": {},
        }
        with contextlib.suppress(BrokenPipeError, ConnectionResetError):
            process.stdin.write(f"{json.dumps(cancel, separators=(',', ':'))}\n".encode())
            await process.stdin.drain()

    @staticmethod
    async def _stop_process(process: asyncio.subprocess.Process) -> None:
        if process.returncode is not None:
            return
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGTERM)
        try:
            await asyncio.wait_for(process.wait(), timeout=2)
        except TimeoutError:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(process.pid, signal.SIGKILL)
            await process.wait()

    @staticmethod
    def _read_single_jsonl(path: Path) -> dict[str, Any]:
        items = HarnessAgent._read_jsonl(path)
        if len(items) != 1:
            raise RuntimeError(f"expected one JSONL object in {path}, got {len(items)}")
        return items[0]

    @staticmethod
    def _read_jsonl(path: Path) -> list[dict[str, Any]]:
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except OSError as error:
            raise RuntimeError(f"failed to read {path}: {error}") from error
        try:
            values = [json.loads(line) for line in lines if line.strip()]
        except json.JSONDecodeError as error:
            raise RuntimeError(f"invalid JSONL in {path}: {error}") from error
        if not all(isinstance(value, dict) for value in values):
            raise RuntimeError(f"all JSONL values in {path} must be objects")
        return values

    @staticmethod
    def _repository_root() -> Path:
        return Path(__file__).resolve().parents[1]
