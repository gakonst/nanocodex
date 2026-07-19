"""Stock Codex adapter with explicit context parity for A/B evaluations."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from harbor.agents.installed.codex import Codex
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext


class ParityCodexAgent(Codex):
    """Run Codex with the nanocodex prompt and shared eval AGENTS.md exactly."""

    _REMOTE_SYSTEM_PROMPT = "/tmp/nanocodex-system-prompt.md"
    _REMOTE_AGENTS_MD = "/app/AGENTS.md"

    def __init__(
        self,
        *args: Any,
        system_prompt_path: str | Path,
        agents_md_path: str | Path,
        **kwargs: Any,
    ) -> None:
        self._system_prompt_path = self._resolve_context_file(
            system_prompt_path, "system prompt"
        )
        self._agents_md_path = self._resolve_context_file(agents_md_path, "AGENTS.md")
        super().__init__(*args, **kwargs)

    def build_cli_flags(self) -> str:
        flags = super().build_cli_flags()
        prompt_override = f'-c model_instructions_file="{self._REMOTE_SYSTEM_PROMPT}"'
        return " ".join(part for part in (flags, prompt_override) if part)

    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        result = await self.exec_as_agent(
            environment,
            "test ! -e /app/AGENTS.md && test ! -e /app/AGENTS.override.md",
        )
        if result.return_code != 0:
            raise RuntimeError(
                "context-parity eval refuses to replace an existing /app/AGENTS.md "
                "or /app/AGENTS.override.md"
            )
        await environment.upload_file(
            self._system_prompt_path, self._REMOTE_SYSTEM_PROMPT
        )
        await environment.upload_file(self._agents_md_path, self._REMOTE_AGENTS_MD)
        await self.exec_as_root(
            environment,
            f"chmod 0444 {self._REMOTE_SYSTEM_PROMPT} {self._REMOTE_AGENTS_MD}",
        )
        await super().run(instruction, environment, context)

    def populate_context_post_run(self, context: AgentContext) -> None:
        super().populate_context_post_run(context)
        self._verify_model_context()

    def _verify_model_context(self) -> None:
        rollout_files = list((self.logs_dir / "sessions").rglob("rollout-*.jsonl"))
        if len(rollout_files) != 1:
            raise RuntimeError(
                f"expected exactly one Codex rollout, found {len(rollout_files)}"
            )

        events = []
        for line in rollout_files[0].read_text(encoding="utf-8").splitlines():
            if line.strip():
                events.append(json.loads(line))

        expected_prompt = self._system_prompt_path.read_text(encoding="utf-8").strip()
        actual_prompt = next(
            (
                event.get("payload", {}).get("base_instructions", {}).get("text")
                for event in events
                if event.get("type") == "session_meta"
            ),
            None,
        )
        if actual_prompt != expected_prompt:
            raise RuntimeError(
                "Codex did not use the configured nanocodex system prompt byte-for-byte"
            )

        agents_md = self._agents_md_path.read_text(encoding="utf-8")
        expected_agents = (
            "# AGENTS.md instructions for /app\n\n"
            f"<INSTRUCTIONS>\n{agents_md}\n</INSTRUCTIONS>"
        )
        input_texts = [
            block["text"]
            for event in events
            if event.get("type") == "response_item"
            for block in event.get("payload", {}).get("content", [])
            if isinstance(block, dict) and isinstance(block.get("text"), str)
        ]
        if expected_agents not in input_texts:
            raise RuntimeError(
                "Codex did not use the configured AGENTS.md byte-for-byte"
            )

    @staticmethod
    def _resolve_context_file(path: str | Path, description: str) -> Path:
        resolved = Path(path).resolve()
        if not resolved.is_file():
            raise ValueError(f"{description} file does not exist: {resolved}")
        return resolved
