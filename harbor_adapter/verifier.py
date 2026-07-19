"""Fast local verifier adapters that preserve benchmark assertions."""

import shlex
from typing import override

from harbor.models.trial.paths import EnvironmentPaths
from harbor.models.verifier.result import VerifierResult
from harbor.utils.env import resolve_env_vars
from harbor.verifier.verifier import Verifier

_BASE_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/base"
_NUMPY_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/numpy"
_POV_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/pov"
_PARQUET_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/parquet"
_SCIENTIFIC_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/scientific"
_PORTFOLIO_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/portfolio"
_ADAPTIVE_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/adaptive"
_MUJOCO_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/mujoco"
_TORCH270_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/torch270"
_TRANSFORMERS455_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/transformers455"
_PYTEST834_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/pytest834"
_PYTEST842_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/pytest842"
_REQUESTS2324_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/requests2324"
_BROWSER_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/browser"
_FILTER_JS_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/filter-js"
_DATASETS360_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/datasets360"
_PILLOW1121_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/pillow1121"
_BN_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/bn"
_SETUPTOOLS809_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/setuptools809"
_PYTORCH_CLI_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/pytorch-cli"
_INSTALL_WINDOWS_311_VERIFIER_SITE_PACKAGES = (
    "/opt/nanocodex-verifier/install-windows-311"
)
_FASTTEXT_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/fasttext"
_SAM_CELL_SEG_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/sam-cell-seg"
_PYPI_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/pypi"
_GITPYTHON_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/gitpython"
_BIOPYTHON_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/biopython"
_CHESS_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/chess"
_VIDEO_VERIFIER_SITE_PACKAGES = "/opt/nanocodex-verifier/video"
_MANAGED_VERIFIER_PYTHON = "/opt/nanocodex-verifier/python"
_MANAGED_VERIFIER_PYTHON_311 = "/opt/nanocodex-verifier/python3.11"
_TASK_VERIFIER_PYTHON = "/opt/nanocodex-verifier/bin/python"
_VERIFIER_OVERLAY_PATH_VALIDATION = (
    "verifier_overlay_ifs=$IFS; IFS=:; "
    "for verifier_overlay_path in $verifier_pythonpath; do "
    'if [ ! -d "$verifier_overlay_path" ]; then '
    'echo "missing verifier ABI overlay: $verifier_overlay_path" >&2; '
    "IFS=$verifier_overlay_ifs; return 127; fi; "
    "done; IFS=$verifier_overlay_ifs; "
)


def _toolbox_library_path_setup_command(
    verifier_library_path: str = "/opt/nanocodex-verifier/lib",
) -> str:
    library_path = shlex.quote(verifier_library_path)
    return (
        f"verifier_toolbox_library_path={library_path}; "
        'if [ ! -d "$verifier_toolbox_library_path" ]; then '
        'echo "missing verifier private library directory: '
        '$verifier_toolbox_library_path" >&2; exit 127; fi; '
        "export verifier_toolbox_library_path"
    )


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
            "export PATH=$PATH:/opt/nanocodex-verifier/bin",
            _toolbox_library_path_setup_command(),
            'verifier_original_pythonpath=${PYTHONPATH:-}',
            'verifier_python_minor=$(python -c \'import sys; '
            'print(f"{sys.version_info[0]}.{sys.version_info[1]}")\')',
            'verifier_task_pythonpath=$(python -c \'import sys; '
            'print(":".join(path for path in sys.path '
            'if "site-packages" in path))\')',
            f'verifier_base_pythonpath={_BASE_VERIFIER_SITE_PACKAGES}/'
            '"$verifier_python_minor"',
            'if [ ! -d "$verifier_base_pythonpath" ]; then '
            'echo "unsupported verifier Python ABI: $verifier_python_minor" >&2; '
            "exit 127; fi",
            f'verifier_uvx_python={_MANAGED_VERIFIER_PYTHON}',
            f'verifier_uvx_base_pythonpath={_BASE_VERIFIER_SITE_PACKAGES}/3.13',
            'if [ ! -x "$verifier_uvx_python" ] || '
            '[ ! -d "$verifier_uvx_base_pythonpath" ]; then '
            'echo "missing managed Python 3.13 verifier runtime" >&2; exit 127; fi',
            'export PYTHONPATH="$verifier_base_pythonpath'
            '${verifier_original_pythonpath:+:$verifier_original_pythonpath}"',
            "export verifier_original_pythonpath verifier_python_minor "
            "verifier_task_pythonpath verifier_base_pythonpath "
            "verifier_uvx_python verifier_uvx_base_pythonpath",
            "if [ -x /usr/bin/chromedriver ]; then "
            "export SE_CHROMEDRIVER=/usr/bin/chromedriver; fi",
            "apt-get() { "
            'case "$*" in '
            '"update"|"update -qq"|"install -y curl"|"install -y vim"|'
            '"install -y expect"|'
            '"install -y curl imagemagick"|'
            '"install -y curl git"|'
            '"install -y curl expect"|'
            '"install -y curl binutils"|'
            '"install -y curl primer3"|'
            '"install -y curl sshpass"|'
            '"install -y curl python3-pillow"|'
            '"install -y curl ffmpeg libsm6 libxext6"|'
            '"install -y curl gcc"|'
            '"install -y curl git libgl1"|'
            '"install -y expect curl"|'
            '"install -y mtools socat vncsnapshot tesseract-ocr"|'
            '"install -y curl expect git openssh-client") return 0 ;; '
            '*) echo "unsupported cached apt-get command: $*" >&2; return 127 ;; '
            "esac; "
            "}",
            "curl() { "
            'if [ "$#" -eq 2 ] && [ "$1" = "-LsSf" ] && '
            '[ "$2" = "https://astral.sh/uv/0.9.5/install.sh" ]; then '
            "return 0; fi; "
            'command curl "$@"; '
            "}",
            "pip() { local verifier_pip_overlay=; "
            'case "$*" in '
            '"install pytest==8.4.1 pytest-json-ctrf==0.3.5"|'
            '"install pytest==8.4.1 pytest-json-ctrf==0.3.5 '
            '--break-system-packages"|'
            '"install pytest==8.4.1 requests==2.32.5 '
            'pytest-json-ctrf==0.3.5") ;; '
            '"install pytest==8.4.2 requests==2.32.5 psutil==7.0.0 '
            'pytest-json-ctrf==0.3.5") '
            f'verifier_pip_overlay={_PYTEST842_VERIFIER_SITE_PACKAGES}/'
            '$verifier_python_minor ;; '
            '"install pytest==8.4.1 psutil==7.0.0 requests==2.32.4 '
            'pytest-json-ctrf==0.3.5") '
            f'verifier_pip_overlay={_REQUESTS2324_VERIFIER_SITE_PACKAGES}/'
            '$verifier_python_minor ;; '
            '*) echo "unsupported cached pip command: $*" >&2; return 127 ;; '
            "esac; "
            'if [ -n "$verifier_pip_overlay" ]; then '
            'if [ ! -d "$verifier_pip_overlay" ]; then '
            'echo "missing verifier pip overlay: $verifier_pip_overlay" >&2; '
            'return 127; fi; '
            'export PYTHONPATH="$verifier_pip_overlay:'
            '$verifier_base_pythonpath'
            '${verifier_original_pythonpath:+:$verifier_original_pythonpath}"; '
            "fi; "
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
            "local verifier_pythonpath= verifier_overlay_path= "
            "verifier_overlay_ifs=; "
            'case "$*" in '
            '"-p 3.13 -w pytest==8.4.1 -w pandas==2.3.3 '
            '-w pyarrow==22.0.0 -w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_PARQUET_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w numpy==2.3.4 '
            '-w pandas==2.3.3 -w matplotlib==3.10.7 '
            '-w scipy==1.16.3 -w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_SCIENTIFIC_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w numpy==2.3.2 '
            '-w setuptools==78.1.1 -w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_PORTFOLIO_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w numpy==2.3.3 '
            '-w scipy==1.16.2 -w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_ADAPTIVE_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.3.4 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_PYTEST834_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w pytest-json-ctrf==0.3.5 pytest "*) ;; '
            '"-p 3.13 -w pytest==8.4.1 -w pip==25.2 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_PYPI_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w numpy==2.3.0 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_NUMPY_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w numpy==2.3.1 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_POV_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w pillow==11.1.0 '
            '-w numpy==2.3.1 -w scikit-image==0.25.0 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_POV_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w mujoco==3.3.5 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_MUJOCO_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w torch==2.7.1 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) ;; '
            '"-p 3.13 -w pytest==8.4.1 -w torch==2.7.0 '
            '-w transformers==4.55.0 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_TRANSFORMERS455_VERIFIER_SITE_PACKAGES}/3.13:'
            f'{_TORCH270_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w torch==2.7.0 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_TORCH270_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w gitpython==3.1.44 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_GITPYTHON_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w biopython==1.85 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_BIOPYTHON_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w chess==1.11.2 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_CHESS_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w toml==0.10.2 '
            '-w opencv-contrib-python==4.11.0.86 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_VIDEO_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w selenium==4.35.0 '
            '-w beautifulsoup4==4.13.5 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_BROWSER_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w selenium==4.38.0 '
            '-w bs4==0.0.2 -w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_FILTER_JS_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w datasets==3.6.0 '
            '-w tqdm==4.67.1 -w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_DATASETS360_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w mteb==1.36.8 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) ;; '
            '"-p 3.13 -w pytest==8.4.1 -w numpy==2.3.1 '
            '-w pillow==11.2.1 -w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_PILLOW1121_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w pillow==11.2.1 '
            '-w numpy==2.3.1 -w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_PILLOW1121_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w pandas==2.3.2 '
            '-w scipy==1.16.1 -w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_BN_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w pandas==2.3.2 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_BN_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w setuptools==80.9.0 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_SETUPTOOLS809_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"--index https://download.pytorch.org/whl/cpu '
            '--index-strategy unsafe-best-match -p 3.13 '
            '-w torch==2.7.1 -w torchvision==0.22.1 '
            '-w numpy==2.3.1 -w opencv-python==4.11.0.86 '
            '-w pytest==8.4.1 -w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_PYTORCH_CLI_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.13 -w pytest==8.4.1 '
            '-w opencv-python==4.11.0.86 -w numpy==2.3.1 '
            '-w pytesseract==0.3.13 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath='
            f'{_INSTALL_WINDOWS_311_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '"-p 3.11 -w pytest==8.4.1 -w scikit-learn==1.7.0 '
            '-w fasttext-wheel==0.9.2 -w numpy==1.24.0 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_FASTTEXT_VERIFIER_SITE_PACKAGES}/3.11; '
            f'verifier_uvx_python={_MANAGED_VERIFIER_PYTHON_311}; '
            f'verifier_uvx_base_pythonpath={_BASE_VERIFIER_SITE_PACKAGES}/3.11 ;; '
            '"--index https://download.pytorch.org/whl/cpu '
            '--index-strategy unsafe-best-match -p 3.11 '
            '-w torch==2.5.1 -w torchvision==0.20.1 '
            '-w pandas==2.3.2 -w timm==1.0.19 '
            '-w opencv-python==4.12.0.88 -w shapely==2.1.1 '
            '-w pytest==8.4.1 -w pytest-json-ctrf==0.3.5 '
            '-w git+https://github.com/ChaoningZhang/MobileSAM.git@'
            '34bbbfdface3c18e5221aa7de6032d7220c6c6a1 pytest "*) '
            f'verifier_pythonpath={_SAM_CELL_SEG_VERIFIER_SITE_PACKAGES}/3.11; '
            f'verifier_uvx_python={_TASK_VERIFIER_PYTHON}; '
            f'verifier_uvx_base_pythonpath={_BASE_VERIFIER_SITE_PACKAGES}/3.11 ;; '
            '"-p 3.13 -w pytest==8.4.1 -w rdflib==7.1.4 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) ;; '
            '"-p 3.13 -w pytest==8.4.1 -w requests==2.32.4 '
            '-w pytest-json-ctrf==0.3.5 pytest "*) '
            f'verifier_pythonpath={_REQUESTS2324_VERIFIER_SITE_PACKAGES}/3.13 ;; '
            '*) echo "unsupported cached uvx command: $*" >&2; return 127 ;; '
            "esac; "
            'while [ "$#" -gt 0 ] && [ "$1" != pytest ]; do shift; done; '
            "shift; "
            'if [ ! -x "$verifier_uvx_python" ] || '
            '[ ! -d "$verifier_uvx_base_pythonpath" ]; then '
            'echo "missing verifier runtime: $verifier_uvx_python" >&2; '
            "return 127; fi; "
            'if [ -n "$verifier_pythonpath" ]; then '
            f"{_VERIFIER_OVERLAY_PATH_VALIDATION}"
            "fi; "
            'env LD_LIBRARY_PATH="${LD_LIBRARY_PATH:+$LD_LIBRARY_PATH:}'
            '$verifier_toolbox_library_path" '
            'PYTHONPATH="${verifier_pythonpath:+$verifier_pythonpath:}'
            '$verifier_uvx_base_pythonpath'
            '${verifier_task_pythonpath:+:$verifier_task_pythonpath}'
            '${verifier_original_pythonpath:+:$verifier_original_pythonpath}" '
            '"$verifier_uvx_python" -m pytest "$@"; '
            "}",
            "export -f apt-get curl pip source pytest uvx",
            "verifier_linked_expect=0",
            "verifier_cleanup() { "
            'if [ "$verifier_linked_expect" = 1 ] && '
            '[ "$(readlink /usr/bin/expect)" = '
            '"/opt/nanocodex-verifier/bin/expect" ]; then '
            "rm -f /usr/bin/expect; fi; }",
            "trap verifier_cleanup EXIT",
            "if [ ! -x /usr/bin/expect ]; then "
            "if [ -e /usr/bin/expect ] || [ -L /usr/bin/expect ]; then "
            'echo "unusable existing /usr/bin/expect" >&2; exit 127; fi; '
            "ln -s /opt/nanocodex-verifier/bin/expect /usr/bin/expect; "
            "verifier_linked_expect=1; fi",
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
