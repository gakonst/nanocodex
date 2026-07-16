"""Fast local verifier adapters that preserve benchmark assertions."""

import shlex
from typing import override

from harbor.models.trial.paths import EnvironmentPaths
from harbor.models.verifier.result import VerifierResult
from harbor.utils.env import resolve_env_vars
from harbor.verifier.verifier import Verifier

_POV_VERIFIER_SITE_PACKAGES = "/opt/harness-verifier/pov"


class PytestVerifier(Verifier):
    """Run the canonical verifier script with preinstalled dependencies."""

    @override
    async def verify(self) -> VerifierResult:
        if not self.environment.capabilities.mounted:
            raise RuntimeError("PytestVerifier requires a mounted environment")

        environment_paths = EnvironmentPaths.for_os(self.environment.os)
        test_source_dirs, _, _ = self._resolve_tests()
        for source_dir in test_source_dirs:
            await self.environment.upload_dir(
                source_dir=source_dir,
                target_dir=str(environment_paths.tests_dir),
            )

        test_script = shlex.quote(str(environment_paths.tests_dir / "test.sh"))
        test_stdout = shlex.quote(
            str(environment_paths.verifier_dir / "test-stdout.txt")
        )
        reward_path = shlex.quote(str(environment_paths.reward_text_path))
        ctrf_path = shlex.quote(str(environment_paths.verifier_dir / "ctrf.json"))
        original_ctrf_path = shlex.quote(
            str(environment_paths.verifier_dir / "original-repo-ctrf.json")
        )
        commands = [
            "script_status=0",
            f"rm -f {reward_path} {ctrf_path} {original_ctrf_path}",
            f": > {test_stdout}",
            "export PATH=/opt/harness-verifier/bin:$PATH",
            "if [ -x /usr/bin/chromedriver ]; then "
            "export SE_CHROMEDRIVER=/usr/bin/chromedriver; fi",
            "apt-get() { "
            'case "$*" in '
            '"update"|"install -y curl"|"install -y vim"|'
            '"install -y curl imagemagick"|'
            '"install -y curl git"|'
            '"install -y curl expect"|'
            '"install -y curl primer3"|'
            '"install -y expect curl"|'
            '"install -y curl expect git openssh-client") return 0 ;; '
            '*) echo "unsupported cached apt-get command: $*" >&2; return 127 ;; '
            "esac; "
            "}",
            "curl() { "
            'if [ "$#" -eq 2 ] && [ "$1" = "-LsSf" ] && '
            '[ "$2" = "https://astral.sh/uv/0.9.5/install.sh" ]; then '
            "return 0; fi; "
            'command /usr/bin/curl "$@"; '
            "}",
            "pip() { "
            'case "$*" in '
            '"install pytest==8.4.1 pytest-json-ctrf==0.3.5"|'
            '"install pytest==8.4.1 pytest-json-ctrf==0.3.5 '
            '--break-system-packages"|'
            '"install pytest==8.4.1 requests==2.32.5 '
            'pytest-json-ctrf==0.3.5"|'
            '"install pytest==8.4.2 requests==2.32.5 psutil==7.0.0 '
            'pytest-json-ctrf==0.3.5") return 0 ;; '
            '*) echo "unsupported cached pip command: $*" >&2; return 127 ;; '
            "esac; "
            "}",
            "source() { "
            'if [ "$#" -eq 1 ] && '
            '[ "$1" = "$HOME/.local/bin/env" ]; then return 0; fi; '
            'builtin source "$@"; '
            "}",
            "pytest() { "
            'if [ "$*" != "--ctrf /logs/verifier/ctrf.json '
            '/tests/test_outputs.py -rA" ]; then '
            'echo "unsupported cached pytest command: $*" >&2; return 127; fi; '
            'command python -m pytest "$@"; '
            "}",
            "uvx() { "
            "local verifier_pythonpath=; "
            'case "$*" in '
            '"-p 3.13 -w pytest==8.4.1 -w pytest-json-ctrf==0.3.5 pytest "*|'
            '"-p 3.13 -w pytest==8.4.1 -w pip==25.2 '
            '-w pytest-json-ctrf==0.3.5 pytest "*|'
            '"-p 3.13 -w pytest==8.4.1 -w numpy==2.3.0 '
            '-w pytest-json-ctrf==0.3.5 pytest "*|'
            '"-p 3.13 -w pytest==8.4.1 -w pillow==11.1.0 '
            '-w numpy==2.3.1 -w scikit-image==0.25.0 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_POV_VERIFIER_SITE_PACKAGES} ;; '
            '"-p 3.13 -w pytest==8.4.1 -w mujoco==3.3.5 '
            '-w pytest-json-ctrf==0.3.5 pytest "*|'
            '"-p 3.13 -w pytest==8.4.1 -w rdflib==7.1.4 '
            '-w pytest-json-ctrf==0.3.5 pytest "*|'
            '"-p 3.13 -w pytest==8.4.1 -w requests==2.32.4 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) ;; '
            '*) echo "unsupported cached uvx command: $*" >&2; return 127 ;; '
            "esac; "
            'while [ "$#" -gt 0 ] && [ "$1" != pytest ]; do shift; done; '
            "shift; "
            'if [ -n "$verifier_pythonpath" ]; then '
            'env PYTHONPATH="$verifier_pythonpath${PYTHONPATH:+:$PYTHONPATH}" '
            'python -m pytest "$@"; '
            'else command python -m pytest "$@"; fi; '
            "}",
            "export -f apt-get curl pip source pytest uvx",
            "cd /app",
            f"bash {test_script} >> {test_stdout} 2>&1 || script_status=$?",
            f'if [ ! -s {reward_path} ]; then '
            f'echo "canonical verifier exited $script_status without a reward" '
            f">> {test_stdout}; echo 0 > {reward_path}; fi",
        ]
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
