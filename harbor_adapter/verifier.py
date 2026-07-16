"""Fast local verifier adapters that preserve benchmark assertions."""

import shlex
from typing import override

from harbor.models.trial.paths import EnvironmentPaths
from harbor.models.verifier.result import VerifierResult
from harbor.utils.env import resolve_env_vars
from harbor.verifier.verifier import Verifier


class PytestVerifier(Verifier):
    """Upload canonical tests and support files, then run preinstalled pytest."""

    @override
    async def verify(self) -> VerifierResult:
        if not self.environment.capabilities.mounted:
            raise RuntimeError("PytestVerifier requires a mounted environment")

        environment_paths = EnvironmentPaths.for_os(self.environment.os)
        test_source_dirs, _, test_script = self._resolve_tests()
        for source_dir in test_source_dirs:
            await self.environment.upload_dir(
                source_dir=source_dir,
                target_dir=str(environment_paths.tests_dir),
            )

        test_stdout = environment_paths.verifier_dir / "test-stdout.txt"
        commands = [
            "status=0",
            f": > {test_stdout}",
            "for source in /tests/*; do "
            'case "$source" in '
            "/tests/test.sh|/tests/test_outputs.py) continue ;; "
            "esac; "
            '[ -e "$source" ] && cp -R "$source" /app/; '
            "done",
        ]
        if "original-repo-ctrf.json" in test_script.read_text():
            commands.append(
                "python -m pytest "
                f"--ctrf {environment_paths.verifier_dir}/original-repo-ctrf.json "
                f"-rA >> {test_stdout} 2>&1 || status=$?"
            )
        commands.extend(
            (
                "python -m pytest "
                f"--ctrf {environment_paths.verifier_dir}/ctrf.json "
                f"{shlex.quote(str(environment_paths.tests_dir))} -rA "
                f">> {test_stdout} 2>&1 || status=$?",
                f'if [ "$status" -eq 0 ]; then echo 1; else echo 0; fi '
                f"> {environment_paths.reward_text_path}",
            )
        )
        command = "\n".join(commands)
        merged_env = {
            **self.task.config.verifier.env,
            **(self.verifier_env or {}),
            **self.override_env,
        }
        await self.environment.exec(
            command=command,
            env=resolve_env_vars(merged_env) if merged_env else None,
        )
        return VerifierResult(rewards=self._parse_reward_text())
