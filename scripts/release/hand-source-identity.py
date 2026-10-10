#!/usr/bin/env python3
"""Release-stage source identity of the standalone Hand (nanocodex-hand).

  hand-source-identity.py compute --target T --profile P [--features F]
      [--payload NAME=PATH]... --report FILE      # prints the 64-hex identity
  hand-source-identity.py verify --report FILE --dep-info TARGET_DIR/.../nanocodex-hand.d

compute runs before cargo build; export its output as NANOCODEX_HAND_IDENTITY.
The identity is a SHA-256 over sorted, length-prefixed records of:

* every file of every path package in the Hand package's own dependency
  closure (normal and build edges, all features: a superset), so CLI-only
  packages are naturally outside it; nested packages outside the closure,
  tests/, benches/, examples/ and target/.git/node_modules are skipped;
* the closure's Cargo.lock entries (name, version, source, checksum) and its
  resolved dependency edges and enabled features per node, with path packages
  named by workspace-relative directory;
* workspace inputs outside package directories (manifest, Cargo config,
  linker wrappers, entitlements, payload verifier, macOS helper sources);
* rustc -vV, cargo -V, C/Swift compiler versions, target, profile, features
  and compiler/linker environment;
* the bytes of each declared native payload (e.g. Linux screen helpers).

Git revisions, tags, timestamps and absolute paths are excluded, so an
unchanged Hand keeps its identity across commits. verify runs after the build
and fails unless every workspace file, directory and external file in the
Hand's Cargo dep-info is covered by the hashed inputs or a declared payload:
an input that the identity missed fails the release instead of reusing a
stale Hand.
"""
import argparse, hashlib, json, os, subprocess, sys
from pathlib import Path

SCHEMA = "nanocodex-hand-source-identity-v1"
HAND_BIN = "nanocodex-hand"
SKIP_DIRS = {"target", ".git", "node_modules"}
PACKAGE_SKIP = {"tests", "benches", "examples"}
INPUTS = [
    "Cargo.toml", ".cargo/config.toml", "nanocodex-vm.entitlements",
    "scripts/tests/linux-screen-helpers-bundle.py", "macos/HandMenuBar",
    "scripts/aarch64-unknown-linux-musl-linker", "scripts/aarch64-unknown-linux-musl-ar",
]
OPTIONAL_INPUTS = ["rust-toolchain.toml", "rust-toolchain"]
ENV_EXACT = {
    "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS", "RUSTC", "RUSTC_WRAPPER_FLAGS",
    "CC", "CXX", "AR", "CFLAGS", "CXXFLAGS", "LDFLAGS", "MACOSX_DEPLOYMENT_TARGET", "SDKROOT",
    "CARGO_INCREMENTAL",
}
ENV_PREFIXES = ("CARGO_PROFILE_", "CARGO_TARGET_", "CC_", "CXX_", "AR_", "CFLAGS_", "CXXFLAGS_", "TARGET_C")
# Output locations, not build inputs: absolute and checkout-specific.
ENV_EXCLUDED = {"CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"}


def fail(message):
    print(f"hand-source-identity: {message}", file=sys.stderr)
    sys.exit(1)


def run(args, cwd, optional=False):
    try:
        return subprocess.run(args, cwd=cwd, check=True, capture_output=True).stdout
    except (OSError, subprocess.CalledProcessError) as error:
        if optional:
            return None
        fail(f"{' '.join(args)} failed: {error}")


class Records:
    def __init__(self):
        self.items = {}

    def add(self, kind, name, value):
        if isinstance(value, str):
            value = value.encode()
        key = (kind, name)
        if key in self.items and self.items[key] != value:
            fail(f"conflicting {kind} record {name}")
        self.items[key] = value

    def digest(self):
        h = hashlib.sha256()
        for (kind, name), value in sorted(self.items.items()):
            for part in (kind.encode(), name.encode(), value):
                h.update(len(part).to_bytes(8, "little"))
                h.update(part)
        return h.hexdigest()


def rel(root, path):
    return Path(os.path.relpath(path, root)).as_posix()


def hash_tree(root, directory, records, files, stop_dirs, package):
    if directory.is_file():
        records.add("file", rel(root, directory), directory.read_bytes())
        files.add(rel(root, directory))
        return
    for current, dirs, names in os.walk(directory, followlinks=False):
        here = Path(current)
        kept = []
        for name in sorted(dirs):
            child = here / name
            if name in SKIP_DIRS or child.resolve() in stop_dirs:
                continue
            if package and here == directory and name in PACKAGE_SKIP:
                continue
            if package and (child / "Cargo.toml").is_file():
                continue  # nested package: hashed only if it is in the closure itself
            kept.append(name)
        dirs[:] = kept
        for name in sorted(names):
            path = here / name
            if path.is_symlink() and not path.exists():
                records.add("symlink", rel(root, path), os.readlink(path))
                continue
            records.add("file", rel(root, path), path.read_bytes())
            files.add(rel(root, path))


def lock_entries(root):
    entries, current = {}, None
    for line in (root / "Cargo.lock").read_text().splitlines():
        if line.strip() == "[[package]]":
            current = {}
        elif current is not None and " = " in line and not line.startswith(" "):
            key, value = line.split(" = ", 1)
            if key in {"name", "version", "source", "checksum"}:
                current[key] = value.strip().strip('"')
                if {"name", "version"} <= current.keys():
                    entries[(current["name"], current["version"], current.get("source", ""))] = current
    return entries


def compute(args):
    root = Path(run(["git", "rev-parse", "--show-toplevel"], Path.cwd()).decode().strip()).resolve()
    metadata = json.loads(run(["cargo", "metadata", "--locked", "--format-version", "1", "--all-features"], root))
    packages = {p["id"]: p for p in metadata["packages"]}
    hands = [p for p in metadata["packages"] if p["source"] is None
             and any(t["name"] == HAND_BIN and "bin" in t["kind"] for t in p["targets"])]
    if len(hands) != 1:
        fail(f"expected exactly one workspace package with bin {HAND_BIN}, found {len(hands)}")
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    closure, stack = set(), [hands[0]["id"]]
    while stack:
        node = stack.pop()
        if node in closure:
            continue
        closure.add(node)
        for dep in nodes[node]["deps"]:
            if any(k.get("kind") != "dev" for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    records, files = Records(), set()
    records.add("schema", SCHEMA, SCHEMA)
    records.add("root", "package", hands[0]["name"])
    lock = lock_entries(root)
    path_dirs = sorted(Path(packages[i]["manifest_path"]).parent.resolve() for i in closure if packages[i]["source"] is None)
    for directory in path_dirs:
        if root not in directory.parents and directory != root:
            fail(f"path package outside the workspace: {directory}")
    for package_id in sorted(closure):
        package = packages[package_id]
        if package["source"] is None:
            continue
        entry = lock.get((package["name"], package["version"], package["source"]))
        if entry is None:
            fail(f"Cargo.lock has no entry for {package['name']} {package['version']}")
        records.add("lock", f"{package['name']} {package['version']} {package['source']}", entry.get("checksum", "git"))
    def node_key(package_id):
        package = packages[package_id]
        if package["source"] is None:
            location = "path:" + rel(root, Path(package["manifest_path"]).parent.resolve())
        else:
            location = package["source"]
        return f"{package['name']} {package['version']} {location}"

    for package_id in closure:
        edges = []
        for dep in nodes[package_id]["deps"]:
            kinds = sorted(f"{k.get('kind') or 'normal'}:{k.get('target') or ''}"
                           for k in dep["dep_kinds"] if k.get("kind") != "dev")
            if kinds:
                edges.append(f"{dep['name']} -> {node_key(dep['pkg'])} [{','.join(kinds)}]")
        records.add("edges", node_key(package_id), "\n".join(sorted(edges)))
        records.add("features", node_key(package_id), ",".join(sorted(nodes[package_id].get("features", []))))
    for directory in path_dirs:
        records.add("package", rel(root, directory), packages[next(i for i in closure if Path(packages[i]["manifest_path"]).parent.resolve() == directory)]["name"])
        hash_tree(root, directory, records, files, set(), True)
    for name in INPUTS + OPTIONAL_INPUTS:
        path = root / name
        if not path.exists():
            if name in OPTIONAL_INPUTS:
                continue
            fail(f"required Hand input {name} is missing; update {Path(__file__).name}")
        hash_tree(root, path, records, files, set(), False)
    records.add("build", "target", args.target)
    records.add("build", "profile", args.profile)
    records.add("build", "features", ",".join(sorted(f for f in args.features.replace(",", " ").split() if f)))
    records.add("tool", "rustc -vV", run([os.environ.get("RUSTC", "rustc"), "-vV"], root))
    records.add("tool", "cargo -V", run(["cargo", "-V"], root))
    for tool in (["cc", "--version"], ["xcrun", "swiftc", "--version"]):
        output = run(tool, root, optional=True)
        records.add("tool", " ".join(tool), output if output is not None else b"absent")
    for name, value in os.environ.items():
        if name not in ENV_EXCLUDED and (name in ENV_EXACT or name.startswith(ENV_PREFIXES)):
            records.add("env", name, value)
    payloads = {}
    for item in args.payload:
        name, _, path = item.partition("=")
        path = Path(path).resolve()
        if not name or not path.is_file() or path.stat().st_size == 0:
            fail(f"payload {item!r} must be NAME=PATH to a non-empty file")
        records.add("payload", name, path.read_bytes())
        payloads[name] = str(path)
    identity = records.digest()
    # Cargo.lock is covered by the closure's lock, edge and feature records
    # rather than its bytes, so lockfile changes outside the Hand closure keep
    # the identity while any resolution change inside it changes it.
    report = {"schema": SCHEMA, "identity": identity, "root": str(root), "package": hands[0]["name"],
              "files": sorted(files | {"Cargo.lock"}), "payloads": payloads,
              "records": [[k, n, hashlib.sha256(v).hexdigest()] for (k, n), v in sorted(records.items.items())]}
    Path(args.report).write_text(json.dumps(report, indent=1) + "\n")
    print(identity)


def dep_info_paths(path):
    text = Path(path).read_text().replace("\\\n", " ")
    out = []
    for line in text.splitlines():
        if ": " not in line:
            continue
        deps = line.split(": ", 1)[1]
        current, escaped = "", False
        for char in deps:
            if escaped:
                current, escaped = current + char, False
            elif char == "\\":
                escaped = True
            elif char == " ":
                if current:
                    out.append(current)
                current = ""
            else:
                current += char
        if current:
            out.append(current)
    return out


def verify(args):
    report = json.loads(Path(args.report).read_text())
    root = Path(report["root"])
    files = set(report["files"])
    payloads = {str(Path(p).resolve()) for p in report["payloads"].values()}
    sysroot = Path(run([os.environ.get("RUSTC", "rustc"), "--print", "sysroot"], root).decode().strip()).resolve()
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")).resolve()
    target_dir = Path(json.loads(run(["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"], root))["target_directory"]).resolve()
    uncovered = []
    for raw in dep_info_paths(args.dep_info):
        path = Path(os.path.normpath(raw if os.path.isabs(raw) else root / raw))
        if not path.exists():
            continue  # an absent optional input contributed nothing
        resolved = path.resolve()
        if str(resolved) in payloads:
            continue
        if any(base == resolved or base in resolved.parents for base in (target_dir, sysroot, cargo_home)):
            continue  # generated outputs, toolchain and locked registry sources
        if root not in resolved.parents and resolved != root:
            uncovered.append(str(resolved))
            continue
        candidates = [resolved] if resolved.is_file() else [p for p in resolved.rglob("*") if p.is_file()
                                                             and not SKIP_DIRS & set(p.relative_to(resolved).parts)]
        uncovered += [rel(root, p) for p in candidates if rel(root, p) not in files]
    if uncovered:
        listed = "\n  ".join(sorted(set(uncovered))[:50])
        fail(f"{len(set(uncovered))} Hand build inputs are not covered by identity {report['identity']}:\n  {listed}")
    print(f"Hand identity {report['identity']} covers every input in {args.dep_info}")


parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
commands = parser.add_subparsers(dest="command", required=True)
c = commands.add_parser("compute")
c.add_argument("--target", required=True)
c.add_argument("--profile", required=True)
c.add_argument("--features", default="")
c.add_argument("--payload", action="append", default=[])
c.add_argument("--report", required=True)
v = commands.add_parser("verify")
v.add_argument("--report", required=True)
v.add_argument("--dep-info", required=True)
parsed = parser.parse_args()
compute(parsed) if parsed.command == "compute" else verify(parsed)
