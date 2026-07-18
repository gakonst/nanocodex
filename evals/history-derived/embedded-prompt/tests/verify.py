from pathlib import Path
import subprocess


root = Path("/app")
markdown = list((root / "src").glob("*.md"))
assert len(markdown) == 1, f"expected exactly one Markdown prompt source, found {markdown}"

rust = "\n".join(path.read_text() for path in (root / "src").glob("*.rs"))
assert "include_str!" in rust, "prompt must be embedded at compile time"
assert "You are a careful coding agent." not in rust, "prompt prose remains in Rust"
assert "std::fs" not in rust and "read_to_string" not in rust, "runtime file I/O is forbidden"

tests = root / "tests"
tests.mkdir(exist_ok=True)
hidden = tests / "hidden.rs"
hidden.write_bytes(Path("/tests/hidden.rs").read_bytes())
try:
    subprocess.run(["cargo", "test", "--quiet"], cwd=root, check=True, timeout=90)
finally:
    hidden.unlink(missing_ok=True)
