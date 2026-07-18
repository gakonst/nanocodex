from pathlib import Path
import importlib.util
import sys


root = Path("/app")
assert not (root / "legacy_parser.py").exists(), "obsolete parser must be deleted"
sys.path.insert(0, str(root))

production = [
    path
    for path in root.glob("*.py")
    if not path.name.startswith("test_")
]
source = "\n".join(path.read_text() for path in production)
assert "legacy_parser" not in source, "legacy compatibility path remains"

nonblank_lines = sum(
    bool(line.strip())
    for path in production
    for line in path.read_text().splitlines()
)
assert nonblank_lines < 26, f"cleanup did not reduce production LOC: {nonblank_lines}"

spec = importlib.util.spec_from_file_location("candidate_parser", root / "parser_api.py")
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)

assert module.parse_records([
    '"alpha,beta", 7\n',
    "\n",
    "  gamma  , -2\n",
]) == [
    {"name": "alpha,beta", "value": 7},
    {"name": "gamma", "value": -2},
]
