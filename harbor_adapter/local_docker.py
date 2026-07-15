"""Host-specific Harbor Docker fast path for the local eval loop."""

import shutil
import sys
from typing import override

from harbor.environments.base import ExecResult, OutputCallback
from harbor.environments.docker.docker import DockerEnvironment


class LocalDockerEnvironment(DockerEnvironment):
    """Avoid Linux ownership and daemon-mode probes on a macOS Docker host.

    Harbor's bind-mount ownership repair is needed on native Linux, but Docker
    Desktop and OrbStack map bind-mount ownership through their VM layer. The
    upstream implementation documents that distinction but currently still
    performs six container-side ``chown`` calls per trial on macOS.

    The Just recipe has already inspected the exact eval image, which proves
    the local Linux Docker daemon is reachable. Repeating the generic daemon
    preflight and mode probe inside Harbor adds two ``docker info`` processes.

    Local jobs also have a fresh random Compose project name and retries are
    disabled. There can be no stale project to remove before ``compose up``.
    Harbor's image-OS probe targets the task's original image even when the
    local Compose overlay replaces it, so that probe is both redundant and
    aimed at the wrong image here.
    """

    _skip_initial_down = True

    @classmethod
    @override
    def preflight(cls) -> None:
        if sys.platform != "darwin":
            return super().preflight()
        if shutil.which("docker") is None:
            raise SystemExit("Docker is not installed or not on PATH")

    @override
    def _validate_daemon_mode(self) -> None:
        if sys.platform != "darwin":
            super()._validate_daemon_mode()

    @override
    async def _validate_image_os(self, image_name: str) -> None:
        if sys.platform != "darwin":
            await super()._validate_image_os(image_name)

    @override
    async def _run_docker_compose_command(
        self,
        command: list[str],
        check: bool = True,
        timeout_sec: int | None = None,
        stdin_data: bytes | None = None,
        on_output: OutputCallback | None = None,
    ) -> ExecResult:
        if (
            sys.platform == "darwin"
            and self._skip_initial_down
            and command == ["down", "--remove-orphans"]
        ):
            self._skip_initial_down = False
            return ExecResult(stdout=None, stderr=None, return_code=0)
        return await super()._run_docker_compose_command(
            command,
            check=check,
            timeout_sec=timeout_sec,
            stdin_data=stdin_data,
            on_output=on_output,
        )

    @override
    async def prepare_logs_for_host(self) -> None:
        if sys.platform != "darwin":
            await super().prepare_logs_for_host()
