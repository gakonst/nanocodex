"""Docker optimizations for the local Harbor loop."""

import asyncio
import shlex
from pathlib import Path
from typing import Any, override

from harbor.environments.docker.docker import DockerEnvironment
from harbor.environments.docker.utils import (
    default_docker_platform,
    ensure_docker_image_built,
)
from harbor.models.trial.config import ServiceVolumeConfig

_TOOLBOX_ROOT = "/opt/nanocodex-toolbox"
_VERIFIER_ROOT = "/opt/nanocodex-verifier"
_TOOLBOX_BUILD_LOCK = asyncio.Lock()
_TOOLBOX_IMAGES: dict[tuple[Path, str], str] = {}


def _toolbox_mount_setup_command(
    *,
    toolbox_root: str = _TOOLBOX_ROOT,
    verifier_root: str = _VERIFIER_ROOT,
    node_modules_root: str = "/usr/share/nodejs",
) -> str:
    toolbox_verifier_root = f"{toolbox_root}{_VERIFIER_ROOT}"
    toolbox_node_modules = f"{toolbox_root}/usr/share/nodejs"
    verifier = shlex.quote(verifier_root)
    toolbox_verifier = shlex.quote(toolbox_verifier_root)
    node_modules = shlex.quote(node_modules_root)
    toolbox_modules = shlex.quote(toolbox_node_modules)
    return (
        f"if [ -e {verifier} ] || [ -L {verifier} ]; then "
        f'test "$(readlink {verifier})" = {toolbox_verifier}; '
        f"else ln -s {toolbox_verifier} {verifier}; fi; "
        f"if [ ! -e {node_modules} ] && [ ! -L {node_modules} ]; then "
        f"ln -s {toolbox_modules} {node_modules}; "
        f"elif [ -d {node_modules} ] && [ ! -L {node_modules} ]; then "
        f"for toolbox_node_entry in {toolbox_modules}/*; do "
        '[ -e "$toolbox_node_entry" ] || continue; '
        f'task_node_entry={node_modules}/${{toolbox_node_entry##*/}}; '
        'if [ ! -e "$task_node_entry" ] && [ ! -L "$task_node_entry" ]; then '
        'ln -s "$toolbox_node_entry" "$task_node_entry"; fi; '
        "done; fi"
    )


class FastDockerEnvironment(DockerEnvironment):
    """Cache native task images and mount one shared verifier toolbox."""

    def __init__(
        self,
        *args: Any,
        toolbox_dockerfile: str | None = "evals/pytest/Dockerfile",
        **kwargs: Any,
    ) -> None:
        super().__init__(*args, **kwargs)
        self._toolbox_dockerfile = (
            Path(toolbox_dockerfile).resolve() if toolbox_dockerfile else None
        )

    @override
    async def start(self, force_build: bool) -> None:
        if self._toolbox_dockerfile is not None:
            task_dockerfile = self.environment_dir / "Dockerfile"
            if not task_dockerfile.is_file():
                raise RuntimeError(
                    "verifier toolbox caching requires the task's environment/Dockerfile"
                )

            platform = await default_docker_platform()

            async def ensure_toolbox_image() -> str:
                key = (self._toolbox_dockerfile, platform)
                async with _TOOLBOX_BUILD_LOCK:
                    if image := _TOOLBOX_IMAGES.get(key):
                        return image
                    image = await ensure_docker_image_built(
                        docker_name="nanocodex/verifier-toolbox",
                        docker_build_context=self._toolbox_dockerfile.parent,
                        dockerfile_path=self._toolbox_dockerfile,
                        build_args={},
                        platform=platform,
                        logger=self.logger,
                    )
                    _TOOLBOX_IMAGES[key] = image
                    return image

            task_image, toolbox_image = await asyncio.gather(
                ensure_docker_image_built(
                    docker_name=f"nanocodex/{self.environment_name}-task",
                    docker_build_context=self.environment_dir,
                    dockerfile_path=task_dockerfile,
                    build_args={},
                    platform=platform,
                    logger=self.logger,
                ),
                ensure_toolbox_image(),
            )
            self._mounts = [
                mount
                for mount in self._mounts
                if mount.get("target") not in {_TOOLBOX_ROOT, _VERIFIER_ROOT}
            ]
            self._mounts.extend(
                [
                    ServiceVolumeConfig(
                        type="image",
                        source=toolbox_image,
                        target=_TOOLBOX_ROOT,
                        read_only=True,
                    )
                ]
            )
            self.task_env_config.docker_image = task_image
            self._env_vars.prebuilt_image_name = task_image
            force_build = False
        await super().start(force_build)
        if self._toolbox_dockerfile is not None:
            await self.exec(
                _toolbox_mount_setup_command(),
                user="root",
            )

    @override
    async def _run_docker_compose_command(
        self, command: list[str], *args: Any, **kwargs: Any
    ) -> Any:
        if command and command[0] in {"down", "stop"}:
            command = [*command, "--timeout", "0"]
        return await super()._run_docker_compose_command(command, *args, **kwargs)
