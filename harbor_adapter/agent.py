"""Install and run the Rust harness inside a Harbor task environment."""

from __future__ import annotations

import json
import shlex
import sys
import tempfile
from pathlib import Path
from typing import Any
from uuid import uuid4

from harbor.agents.installed.base import BaseInstalledAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext
from harbor.models.trajectories import (
    Agent,
    FinalMetrics,
    Metrics,
    Observation,
    ObservationResult,
    Step,
    ToolCall,
    Trajectory,
)
from harbor.utils.trajectory_utils import format_trajectory_json


PROTOCOL_VERSION = 1
TERMINAL_EVENTS = {"run.completed", "run.failed"}
RUN_METRIC_FIELDS = (
    "connection_attempts",
    "websocket_reconnects",
    "connection_duration_ns",
    "model_duration_ns",
    "warmup_duration_ns",
    "tool_work_duration_ns",
    "tool_wall_duration_ns",
)
USAGE_METRIC_FIELDS = ("cache_write_input_tokens", "reasoning_output_tokens")


class HarnessAgent(BaseInstalledAgent):
    """Upload one Rust binary, run it once, and retain its JSONL."""

    SUPPORTS_ATIF = True
    _BINARY = "/installed-agent/harness"
    _INPUT = "/logs/agent/input.jsonl"
    _EVENTS = "/logs/agent/events.jsonl"
    _STDERR = "/logs/agent/stderr.log"
    _API_KEY_FILE = "/installed-agent/.openai-api-key"

    def __init__(
        self,
        logs_dir: Path,
        binary_path: str | Path = ".harness/installed/harness",
        model_name: str | None = None,
        effort: str = "low",
        extra_env: dict[str, str] | None = None,
        **kwargs: Any,
    ) -> None:
        agent_env = dict(extra_env or {})
        self._api_key = agent_env.pop("OPENAI_API_KEY", None)
        if not self._api_key or not self._api_key.strip():
            raise ValueError("OPENAI_API_KEY is required")
        super().__init__(
            logs_dir=logs_dir,
            model_name=model_name,
            extra_env=agent_env,
            **kwargs,
        )
        self._binary_path = Path(binary_path).resolve()
        self._model = self._api_model_name(model_name)
        self._effort = effort

    @staticmethod
    def name() -> str:
        return "harness"

    def get_version_command(self) -> str:
        return f"{self._BINARY} --version"

    async def install(self, environment: BaseEnvironment) -> None:
        if not self._binary_path.is_file():
            raise RuntimeError(
                f"missing harness binary at {self._binary_path}; "
                "run `just build-agent`"
            )
        await environment.upload_file(self._binary_path, self._BINARY)
        await self.exec_as_root(environment, f"chmod 0755 {self._BINARY}")

    async def _stage_api_key(self, environment: BaseEnvironment) -> None:
        identity = await self.exec_as_agent(environment, "id -u")
        user_id = (identity.stdout or "").strip()
        if not user_id.isdecimal():
            raise RuntimeError("failed to resolve the agent user identifier")

        with tempfile.TemporaryDirectory(prefix="harness-agent-secret-") as directory:
            api_key_path = Path(directory) / "openai-api-key"
            api_key_path.write_text(self._api_key, encoding="utf-8")
            api_key_path.chmod(0o600)
            await environment.upload_file(api_key_path, self._API_KEY_FILE)
        await self.exec_as_root(
            environment,
            f"chown {user_id} {self._API_KEY_FILE} && chmod 0400 {self._API_KEY_FILE}",
        )

    async def _remove_staged_api_key(self, environment: BaseEnvironment) -> None:
        try:
            await self.exec_as_root(environment, f"rm -f {self._API_KEY_FILE}")
        except Exception as error:
            self.logger.warning("failed to remove staged API key: %s", error)

    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        del context
        request = {
            "protocol_version": PROTOCOL_VERSION,
            "request_id": str(self.context_id or self.session_id or uuid4()),
            "seq": 1,
            "type": "task.start",
            "payload": {"instruction": instruction},
        }
        input_path = self.logs_dir / "input.jsonl"
        input_path.write_text(
            json.dumps(request, separators=(",", ":")) + "\n", encoding="utf-8"
        )
        if not environment.capabilities.mounted:
            await environment.upload_file(input_path, self._INPUT)

        arguments = [
            self._BINARY,
            "run",
            "--model",
            self._model,
            "--effort",
            self._effort,
        ]
        command = (
            f"api_key=$(<{self._API_KEY_FILE}) && test -n \"$api_key\" && "
            f"rm -f {self._API_KEY_FILE} && OPENAI_API_KEY=\"$api_key\" "
            + " ".join(shlex.quote(argument) for argument in arguments)
            + f" < {self._INPUT} 2> {self._STDERR} | tee {self._EVENTS}"
        )
        try:
            await self._stage_api_key(environment)
            result = await self.exec_as_agent(environment, command)
        finally:
            await self._remove_staged_api_key(environment)
        if result.stdout:
            print(result.stdout, end="", flush=True)
        if result.stderr:
            print(result.stderr, end="", file=sys.stderr, flush=True)

    def populate_context_post_run(self, context: AgentContext) -> None:
        requests = self._read_jsonl(self.logs_dir / "input.jsonl")
        if len(requests) != 1 or requests[0].get("type") != "task.start":
            raise RuntimeError("input.jsonl must contain one task.start event")
        request = requests[0]
        request_id = request["request_id"]

        events = self._read_jsonl(self.logs_dir / "events.jsonl")
        for seq, event in enumerate(events, start=1):
            if (
                event.get("protocol_version") != PROTOCOL_VERSION
                or event.get("request_id") != request_id
                or event.get("seq") != seq
                or not isinstance(event.get("type"), str)
                or not isinstance(event.get("payload"), dict)
            ):
                raise RuntimeError(f"invalid harness event at sequence {seq}")

        terminals = [event for event in events if event["type"] in TERMINAL_EVENTS]
        if len(terminals) != 1:
            raise RuntimeError(
                f"expected exactly one terminal event, found {len(terminals)}"
            )
        terminal = terminals[0]
        if terminal["seq"] != events[-1]["seq"]:
            raise RuntimeError("the terminal event must be the final event")
        terminal_payload = terminal["payload"]
        model_calls = terminal_payload.get("model_calls", 0)
        tool_calls = sum(event["type"] == "tool.call" for event in events)
        usage = terminal_payload.get("usage")
        usage = usage if isinstance(usage, dict) else {}
        warmup_usage = terminal_payload.get("warmup_usage")
        warmup_usage = warmup_usage if isinstance(warmup_usage, dict) else {}
        runtime_metrics = {
            field: terminal_payload.get(field) for field in RUN_METRIC_FIELDS
        }
        runtime_metrics["warmup_usage"] = warmup_usage
        runtime_metrics.update(
            {field: usage.get(field) for field in USAGE_METRIC_FIELDS}
        )
        input_tokens = self._optional_int(usage.get("input_tokens"))
        cached_tokens = self._optional_int(usage.get("cached_input_tokens"))
        output_tokens = self._optional_int(usage.get("output_tokens"))
        cost_usd = self._optional_float(terminal_payload.get("cost_usd"))
        reasoning = "".join(
            event["payload"].get("text", "")
            for event in events
            if event["type"] == "reasoning.summary.delta"
            and isinstance(event["payload"].get("text"), str)
        )
        atif_tool_calls = self._atif_tool_calls(events)
        observations = self._atif_observations(events, atif_tool_calls)
        message = next(
            (
                event["payload"].get("text", "")
                for event in reversed(events)
                if event["type"] == "assistant.message"
            ),
            "Harness emitted no assistant message.",
        )

        trajectory = Trajectory(
            session_id=request_id,
            agent=Agent(
                name=self.name(),
                version=self.version() or "unknown",
                model_name=terminal_payload.get("model"),
                extra={
                    "transport": terminal_payload.get("transport"),
                    "orchestration": terminal_payload.get("orchestration"),
                },
            ),
            steps=[
                Step(
                    step_id=1,
                    source="user",
                    message=request["payload"]["instruction"],
                ),
                Step(
                    step_id=2,
                    source="agent",
                    message=message,
                    model_name=terminal_payload.get("model"),
                    reasoning_effort=terminal_payload.get("effort"),
                    reasoning_content=reasoning or None,
                    tool_calls=atif_tool_calls or None,
                    observation=(
                        Observation(results=observations) if observations else None
                    ),
                    metrics=Metrics(
                        prompt_tokens=input_tokens,
                        completion_tokens=output_tokens,
                        cached_tokens=cached_tokens,
                        cost_usd=cost_usd,
                        extra=runtime_metrics,
                    )
                    if model_calls
                    else None,
                    llm_call_count=model_calls,
                    extra={
                        "terminal_event_type": terminal["type"],
                        "terminal_payload": terminal_payload,
                    },
                ),
            ],
            notes=None,
            final_metrics=FinalMetrics(
                total_prompt_tokens=input_tokens,
                total_completion_tokens=output_tokens,
                total_cached_tokens=cached_tokens,
                total_cost_usd=cost_usd,
                total_steps=2,
                extra={
                    "model_calls": model_calls,
                    "tool_calls": tool_calls,
                    "duration_ns": terminal_payload.get("duration_ns"),
                    **runtime_metrics,
                },
            ),
        )
        (self.logs_dir / "trajectory.json").write_text(
            format_trajectory_json(trajectory.to_json_dict()), encoding="utf-8"
        )

        context.n_input_tokens = input_tokens
        context.n_cache_tokens = cached_tokens
        context.n_output_tokens = output_tokens
        context.cost_usd = cost_usd
        context.metadata = {
            "protocol_version": PROTOCOL_VERSION,
            "terminal_event_type": terminal["type"],
            "model_calls": model_calls,
            "tool_calls": tool_calls,
            "model": terminal_payload.get("model"),
            "effort": terminal_payload.get("effort"),
            "transport": terminal_payload.get("transport"),
            "orchestration": terminal_payload.get("orchestration"),
            "duration_ms": terminal_payload.get("duration_ms"),
            "duration_ns": terminal_payload.get("duration_ns"),
            **runtime_metrics,
            "last_response_id": terminal_payload.get("last_response_id"),
            "cost_status": terminal_payload.get("cost_status"),
        }

    @staticmethod
    def _api_model_name(model_name: str | None) -> str:
        if model_name is None:
            return "gpt-5.6-sol"
        _, separator, api_model = model_name.partition("/")
        return api_model if separator else model_name

    @staticmethod
    def _optional_int(value: Any) -> int | None:
        return value if isinstance(value, int) and not isinstance(value, bool) else None

    @staticmethod
    def _optional_float(value: Any) -> float | None:
        if isinstance(value, (int, float)) and not isinstance(value, bool):
            return float(value)
        return None

    @staticmethod
    def _atif_tool_calls(events: list[dict[str, Any]]) -> list[ToolCall]:
        calls = []
        for event in events:
            if event["type"] != "tool.call":
                continue
            payload = event["payload"]
            arguments = payload.get("arguments")
            if not isinstance(arguments, dict):
                arguments = {"raw": arguments}
            calls.append(
                ToolCall(
                    tool_call_id=str(payload.get("call_id", "")),
                    function_name=str(payload.get("tool", "")),
                    arguments=arguments,
                    extra={
                        "model_call_index": payload.get("model_call_index"),
                    },
                )
            )
        return calls

    @staticmethod
    def _atif_observations(
        events: list[dict[str, Any]], calls: list[ToolCall]
    ) -> list[ObservationResult]:
        call_ids = {call.tool_call_id for call in calls}
        observations = []
        for event in events:
            if event["type"] != "tool.result":
                continue
            payload = event["payload"]
            call_id = str(payload.get("call_id", ""))
            if call_id not in call_ids:
                continue
            result = payload.get("result", payload)
            observations.append(
                ObservationResult(
                    source_call_id=call_id,
                    content=json.dumps(result, separators=(",", ":")),
                    extra={
                        "status": payload.get("status"),
                        "duration_ns": payload.get("duration_ns"),
                    },
                )
            )
        return observations

    @staticmethod
    def _read_jsonl(path: Path) -> list[dict[str, Any]]:
        try:
            values = [
                json.loads(line)
                for line in path.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
        except (OSError, json.JSONDecodeError) as error:
            raise RuntimeError(f"failed to read JSONL from {path}: {error}") from error
        if not all(isinstance(value, dict) for value in values):
            raise RuntimeError(f"all JSONL values in {path} must be objects")
        return values
