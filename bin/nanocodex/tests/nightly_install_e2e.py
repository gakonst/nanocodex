#!/usr/bin/env python3
"""Published nightly installer/updater journey on Linux, in an isolated prefix.

python3 bin/nanocodex/tests/nightly_install_e2e.py --old-sha OLD --new-sha NEW \
  --output output/nightly-install --steps a1,a2,a3,a4,a5,b1,b2,b3,modes

Downloads real immutable nightly releases over HTTPS with the public installer
(https://nanocodex.paradigm.xyz) and the shipped "nanocodex update --nightly".
Nothing is built or faked. Every CLI/updater process runs inside a private user +
mount + PID namespace with an empty tmpfs /run (no systemd => no Hand owner) and
read-only binds of /opt/nanocodex and the real ~/.nanocodex, under a synthetic
HOME, TMPDIR and NANOCODEX_DIR. Host service receipts are captured outside the
namespace before and after the run and must be identical.

Steps (state is kept in OUTPUT/state.json, so steps can run in separate calls):
  a1  prefix A: installer NANOCODEX_RELEASE_TAG=nightly-OLD (first install)
  a2  prefix A: OLD CLI "update --nightly" downloads NEW (pointer must name NEW)
  a3  prefix A: NEW CLI "update --nightly" again (cached; nothing rewritten)
  a4  prefix A: rollback with installer tag nightly-OLD (cached reactivation)
  a5  prefix A: OLD CLI "update --nightly" (cached NEW reactivation)
  b1  prefix B: installer tag nightly-NEW (fresh install by the NEW updater:
      Hand stored once by identity, voice runtime)
  b2  prefix B: rollback with installer tag nightly-OLD
  b3  prefix B: OLD CLI "update --nightly" then NEW CLI "update --nightly"
  modes  for each prefix with NEW active: ncl run + ncl TUI turn against a
      synthetic loopback Responses server; managed TUI start boundary with an
      empty HOME (may legitimately stop at login); hand status (no owner).
  old-modes  modes on the currently active OLD version of prefix A (the CLI layout,
      pre-unified or unified, is detected from bin/nanocodex run --help).

Independent verification streams each payload once (cached in OUTPUT/verify),
checking SHA256SUMS and comparing the decompressed bytes and voice members with
the installed files; nothing extra is written to disk.
"""
import argparse, gzip, hashlib, io, json, os, re, select, shlex, shutil, signal, subprocess, sys, tarfile
import threading, time, urllib.request, zlib, errno, fcntl, pty, struct, termios
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = "gakonst/nanocodex"
INSTALLER_URL = "https://nanocodex.paradigm.xyz"
TRIPLE = "x86_64-unknown-linux-gnu"
CLI_ASSET = f"nanocodex-{TRIPLE}"
HAND_ASSET = f"nanocodex2-{TRIPLE}"
GUEST_ASSET = "nanocodex-vm-guest-x86_64-unknown-linux-musl"
VOICE_ASSET = f"nanocodex-voice-{TRIPLE}.tar.gz"
CLI_ALIASES = ["nanocodex", "nanocodex2", "nc", "ncl"]
HAND_ALIASES = ["nanocodex-hand", "nc-hand"]
ALL_STEPS = ["a1", "a2", "a3", "a4", "a5", "b1", "b2", "b3", "b4", "modes", "old-modes", "c1", "c2", "final-modes", "p1", "p2"]

ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
ap.add_argument("--old-sha", required=True)
ap.add_argument("--new-sha", required=True)
ap.add_argument("--final-sha", help="a later published nightly for the c1/c2 upgrade steps")
ap.add_argument("--candidate", nargs=3, metavar=("CLI", "HAND", "VOICE_ARCHIVE"), type=Path,
                help="p1/p2: an unpublished candidate pair (built with VERGEN_GIT_SHA and NANOCODEX_HAND_IDENTITY) and voice archive")
ap.add_argument("--output", type=Path, default=Path("output/nightly-install"))
ap.add_argument("--steps", default="a1,a2,a3,a4,a5,b1,b2,b3,b4,modes")
ap.add_argument("--inner", action="store_true", help=argparse.SUPPRESS)
args = ap.parse_args()
for sha in (args.old_sha, args.new_sha) + ((args.final_sha,) if args.final_sha else ()):
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        sys.exit(f"expected a full lowercase 40-hex commit, got {sha!r}")
ART = args.output.absolute()
ART.mkdir(parents=True, exist_ok=True)
STEPS = [s for s in args.steps.split(",") if s]
for s in STEPS:
    if s not in ALL_STEPS:
        sys.exit(f"unknown step {s}")
OLD, NEW, FINAL = args.old_sha, args.new_sha, args.final_sha
CAND = [c.absolute() for c in args.candidate] if args.candidate else None
REAL_HOME = Path(os.environ.get("NIGHTLY_E2E_REAL_HOME") or os.path.expanduser("~"))


def sh(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


# ---------------------------------------------------------------- outer (host)
def host_receipt():
    """Read-only host service evidence; never starts, stops or writes anything."""
    r = {}
    r["unit"] = sh(["systemctl", "show", "nanocodex-hand.service", "--no-pager", "-p",
                    "Id,LoadState,ActiveState,SubState,MainPID,ExecMainStartTimestampMonotonic,NRestarts,FragmentPath,InvocationID"]).stdout
    r["units"] = sh(["systemctl", "list-units", "nanocodex*", "--all", "--no-legend", "--plain", "--no-pager"]).stdout
    r["current"] = os.readlink("/opt/nanocodex/current") if os.path.islink("/opt/nanocodex/current") else None
    try:
        r["hand_sha256"] = hashlib.sha256(Path("/opt/nanocodex/current/nanocodex2").read_bytes()).hexdigest()
    except OSError as e:
        r["hand_sha256"] = f"unreadable: {e}"
    r["opt_listing"] = sh(["ls", "-la", "--time-style=full-iso", "/opt/nanocodex"]).stdout
    try:
        r["unit_file_sha256"] = hashlib.sha256(Path("/etc/systemd/system/nanocodex-hand.service").read_bytes()).hexdigest()
    except OSError as e:
        r["unit_file_sha256"] = f"unreadable: {e}"
    ps = sh(["ps", "-eo", "pid,lstart,args", "--no-headers"]).stdout.splitlines()
    # Match the executable (argv[0], after PID and the 5-field start time) only, so
    # unrelated shells that merely mention these paths are not counted.
    r["hand_processes"] = [l.strip() for l in ps
                           if len(l.split()) > 6 and l.split()[6].startswith(("/opt/nanocodex/", "/mnt/fast/nanocodex-hand-releases/"))]
    real = REAL_HOME / ".nanocodex"
    r["real_store"] = sh(["find", str(real), "-printf", "%P %y %s %i %T@\n"]).stdout if real.exists() else None
    # Writable real-HOME persistence surfaces a misdirected installer could touch.
    for rel in (".config/systemd/user", ".local/bin"):
        d = REAL_HOME / rel
        r["home:" + rel] = sh(["find", str(d), "-maxdepth", "2", "-printf", "%P %y %s %i %T@\n"]).stdout if d.exists() else None
    for rel in (".bashrc", ".profile", ".bash_profile", ".zshenv", ".zshrc", ".config/fish/config.fish"):
        f = REAL_HOME / rel
        r["home:" + rel] = hashlib.sha256(f.read_bytes()).hexdigest() if f.is_file() else None
    c = sh(["crontab", "-l"])
    r["crontab"] = hashlib.sha256((c.stdout + c.stderr).encode()).hexdigest()
    return r


def outer():
    resolv = Path("/etc/resolv.conf").read_text()
    (ART / "resolv.conf").write_text(resolv)
    before = host_receipt()
    (ART / "host-before.json").write_text(json.dumps(before, indent=2))
    real = REAL_HOME / ".nanocodex"
    binds = ["/opt/nanocodex"] + ([str(real)] if real.is_dir() else [])
    ro = " && ".join(f"mount --bind {shlex.quote(p)} {shlex.quote(p)} && mount -o remount,bind,ro {shlex.quote(p)}" for p in binds)
    setup = (
        "set -e; mount --make-rprivate /; mount -t tmpfs tmpfs /run; "
        "mkdir -p /run/systemd/resolve; cp " + shlex.quote(str(ART / "resolv.conf")) + " /run/systemd/resolve/stub-resolv.conf; "
        + ro + "; test ! -e /run/systemd/system; "
        "exec " + shlex.join([sys.executable, os.path.abspath(__file__), "--inner", "--old-sha", OLD, "--new-sha", NEW,
                              *(["--final-sha", FINAL] if FINAL else []),
                              *(["--candidate", *map(str, CAND)] if CAND else []), "--output", str(ART), "--steps", ",".join(STEPS)]))
    env = dict(os.environ, NIGHTLY_E2E_REAL_HOME=str(REAL_HOME), NIGHTLY_E2E_UID=str(os.getuid()), NIGHTLY_E2E_GID=str(os.getgid()))
    started = time.time()
    code = subprocess.call(["unshare", "--user", "--map-root-user", "--mount", "--pid", "--fork", "--mount-proc",
                            "sh", "-c", setup], env=env)
    after = host_receipt()
    (ART / "host-after.json").write_text(json.dumps(after, indent=2))
    diff = {k: {"before": before[k], "after": after[k]} for k in before if before[k] != after[k]}
    (ART / "host-diff.json").write_text(json.dumps(diff, indent=2))
    print(f"inner exit {code} after {time.time() - started:.0f}s; host receipts {'UNCHANGED' if not diff else 'CHANGED: ' + ', '.join(diff)}")
    print(f"evidence: {ART}")
    sys.exit(code if not diff else 3)


# ---------------------------------------------------------------- inner (namespace)
TRANSCRIPT = ART / "transcript.log"
STATE_PATH = ART / "state.json"
state = json.loads(STATE_PATH.read_text()) if STATE_PATH.exists() else {"checks": [], "steps": {}}
cur_step = ["setup"]
counter = [len(list((ART / "cmd").glob("*.json"))) if (ART / "cmd").exists() else 0]


def save():
    STATE_PATH.write_text(json.dumps(state, indent=2))


def log(line):
    with TRANSCRIPT.open("a") as f:
        f.write(line.rstrip("\n") + "\n")


def check(name, ok, **detail):
    entry = {"step": cur_step[0], "check": name, "ok": bool(ok), **detail}
    state["checks"].append(entry)
    save()
    log(f"[{'PASS' if ok else 'FAIL'}] {cur_step[0]}: {name} {json.dumps(detail)[:600] if detail else ''}")
    return bool(ok)


def require(name, ok, **detail):
    if not check(name, ok, **detail):
        raise AssertionError(f"{cur_step[0]}: {name}")


def prefix_paths(name):
    root = ART / f"prefix-{name}"
    p = {"root": root, "home": root / "home", "store": root / "home/.nanocodex", "tmp": root / "tmp", "ws": root / "ws"}
    for k in ("home", "tmp", "ws"):
        p[k].mkdir(parents=True, exist_ok=True)
    p["store"].mkdir(parents=True, exist_ok=True)
    # Explicit automatic-update opt-out: no user timer is written or enabled.
    (p["store"] / "automatic-updates-disabled").touch()
    return p


def base_env(p):
    return {"PATH": "/usr/bin:/bin", "HOME": str(p["home"]), "NANOCODEX_DIR": str(p["store"]),
            "TMPDIR": str(p["tmp"]), "TERM": "xterm-256color", "LANG": "C.UTF-8", "NO_COLOR": "1",
            "NANOCODEX_INSTALL_NO_SETUP": "1", "NANOCODEX_INSTALL_TTY": "/dev/null",
            "NANOCODEX_COMPUTER": "off", "BROWSER": "/bin/false", "SHELL": "/bin/sh"}


def wait_for_api_quota(minimum=6, limit_s=3900):
    """The installer and updater call api.github.com unauthenticated (60/h per IP,
    shared with every process on the host). Wait for the reset instead of reporting
    an exhausted shared quota as a product failure; /rate_limit itself is free."""
    deadline = time.time() + limit_s
    while True:
        try:
            with http_get("https://api.github.com/rate_limit") as r:
                core = json.load(r)["resources"]["core"]
        except Exception as e:  # network trouble is left to the real command
            log(f"  rate-limit probe failed: {e}")
            return
        if core["remaining"] >= minimum or time.time() > deadline:
            return
        pause = max(5, min(60, core["reset"] - time.time() + 5, deadline - time.time()))
        log(f"  GitHub API quota {core['remaining']}/{core['limit']}; waiting {pause:.0f}s for reset {core['reset']}")
        time.sleep(pause)


def run(name, argv, env, cwd=None, timeout=900, input=None):
    if ("update" in argv and "--nightly" in argv) or any(str(a).endswith("public-install.sh") for a in argv):
        wait_for_api_quota()
    counter[0] += 1
    stem = ART / "cmd" / f"{counter[0]:03d}-{cur_step[0]}-{name}"
    stem.parent.mkdir(exist_ok=True)
    log(f"$ [{cur_step[0]}] " + " ".join(f"{k}={shlex.quote(v)}" for k, v in env.items() if k.startswith("NANOCODEX_RELEASE")) + " " + shlex.join(argv))
    t = time.time()
    try:
        r = subprocess.run(argv, env=env, cwd=cwd, capture_output=True, text=True, timeout=timeout, input=input,
                           stdin=None if input is not None else subprocess.DEVNULL)
        code, out, err = r.returncode, r.stdout, r.stderr
    except subprocess.TimeoutExpired as e:
        code, out, err = "timeout", (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or ""), (e.stderr or b"").decode() if isinstance(e.stderr, bytes) else (e.stderr or "")
    dur = round(time.time() - t, 2)
    stem.with_suffix(".out").write_text(out)
    stem.with_suffix(".err").write_text(err)
    stem.with_suffix(".json").write_text(json.dumps({"argv": argv, "env": env, "cwd": str(cwd) if cwd else None,
                                                     "exit": code, "seconds": dur}, indent=2))
    log(f"  -> exit {code} in {dur}s ({stem.name}.out/.err)")
    return {"exit": code, "out": out, "err": err, "seconds": dur, "record": stem.name}


# --- release metadata and independent streamed verification
def http_get(url, accept=None):
    req = urllib.request.Request(url, headers={"User-Agent": "nanocodex-nightly-install-e2e", **({"Accept": accept} if accept else {})})
    return urllib.request.urlopen(req, timeout=60)


def release(tag, refresh=False):
    path = ART / "releases" / f"{tag}.json"
    path.parent.mkdir(exist_ok=True)
    if path.exists() and not refresh:
        return json.loads(path.read_text())
    with http_get(f"https://api.github.com/repos/{REPO}/releases/tags/{tag}", "application/vnd.github+json") as r:
        remaining = r.headers.get("x-ratelimit-remaining")
        data = json.load(r)
    log(f"  GET release {tag}: target {data.get('target_commitish')} (rate limit remaining {remaining})")
    path.write_text(json.dumps(data, indent=2))
    return data


def sums(tag):
    path = ART / "releases" / f"{tag}.SHA256SUMS"
    if not path.exists():
        with http_get(f"https://github.com/{REPO}/releases/download/{tag}/SHA256SUMS") as r:
            path.write_bytes(r.read())
    out = {}
    for line in path.read_text().splitlines():
        parts = line.split()
        if len(parts) == 2:
            out[parts[1].lstrip("*")] = parts[0]
    return out


def asset(rel, name):
    gz = next((a for a in rel["assets"] if a["name"] == name + ".gz"), None)
    return gz or next(a for a in rel["assets"] if a["name"] == name)


def expected_key(sha):
    rel = release(f"nightly-{sha}")
    return f"nightly-{sha}-{asset(rel, CLI_ASSET)['id']}-{asset(rel, HAND_ASSET)['id']}-{asset(rel, GUEST_ASSET)['id']}"


class Hashing(io.RawIOBase):
    def __init__(self, raw):
        self.raw, self.h, self.n = raw, hashlib.sha256(), 0
    def readable(self):
        return True
    def readinto(self, b):
        data = self.raw.read(len(b))
        self.h.update(data); self.n += len(data)
        b[:len(data)] = data
        return len(data)


def verify_release(sha):
    """Stream each Linux payload once; record archive and decompressed digests."""
    tag = f"nightly-{sha}"
    path = ART / "verify" / f"{tag}.json"
    path.parent.mkdir(exist_ok=True)
    if path.exists():
        return json.loads(path.read_text())
    rel, manifest = release(tag), sums(tag)
    result = {"tag": tag, "manifest": manifest, "assets": {}}
    for logical in (CLI_ASSET, HAND_ASSET, GUEST_ASSET):
        a = asset(rel, logical)
        h_arc, h_raw, n = hashlib.sha256(), hashlib.sha256(), 0
        d = zlib.decompressobj(16 + zlib.MAX_WBITS) if a["name"].endswith(".gz") else None
        with http_get(a["browser_download_url"]) as r:
            while chunk := r.read(1 << 20):
                h_arc.update(chunk)
                raw = d.decompress(chunk) if d else chunk
                h_raw.update(raw); n += len(raw)
        if d:
            tail = d.flush(); h_raw.update(tail); n += len(tail)
        result["assets"][logical] = {"asset": a["name"], "id": a["id"], "archive_sha256": h_arc.hexdigest(),
                                     "manifest_sha256": manifest.get(a["name"]), "raw_sha256": h_raw.hexdigest(), "raw_size": n}
    a = asset(rel, VOICE_ASSET)
    members = {}
    with http_get(a["browser_download_url"]) as r:
        hr = Hashing(r)
        with tarfile.open(fileobj=io.BufferedReader(hr), mode="r|gz") as tf:
            for m in tf:
                if m.isfile():
                    members[m.name] = hashlib.sha256(tf.extractfile(m).read()).hexdigest()
        # The tar reader may stop before the gzip trailer; drain the remainder
        # through the hashing reader so the archive digest covers every byte.
        while hr.read(1 << 20):
            pass
    result["assets"][VOICE_ASSET] = {"asset": a["name"], "id": a["id"], "archive_sha256": hr.h.hexdigest(),
                                     "manifest_sha256": manifest.get(a["name"]), "members": members}
    path.write_text(json.dumps(result, indent=2))
    return result


# --- installed store inspection
_sha_cache = {}


def file_sha(path):
    st = path.stat()
    k = (st.st_ino, st.st_size, st.st_mtime_ns)
    if k not in _sha_cache:
        h = hashlib.sha256()
        with path.open("rb") as f:
            while chunk := f.read(1 << 20):
                h.update(chunk)
        _sha_cache[k] = h.hexdigest()
    return _sha_cache[k]


def snapshot(p):
    store = p["store"]
    snap = {"current": os.readlink(store / "current") if (store / "current").is_symlink() else None, "files": {}}
    for top in ("versions", "hand-versions", "bin", "updater"):
        base = store / top
        if not base.exists():
            continue
        for dirpath, dirnames, filenames in os.walk(base):
            dirnames[:] = [d for d in dirnames if not d.startswith(".install-")]
            for name in sorted(dirnames + filenames):
                path = Path(dirpath) / name
                rel = str(path.relative_to(store))
                st = path.lstat()
                if path.is_symlink():
                    snap["files"][rel] = {"type": "link", "target": os.readlink(path)}
                    if name in dirnames:
                        dirnames.remove(name)
                elif path.is_file():
                    snap["files"][rel] = {"type": "file", "size": st.st_size, "inode": st.st_ino, "nlink": st.st_nlink,
                                          "mtime_ns": st.st_mtime_ns, "ctime_ns": st.st_ctime_ns, "sha256": file_sha(path)}
    for name in ("pending-update", "explicit-selection", "update-transaction.json", "automatic-updates-disabled"):
        snap[name] = (store / name).read_text().strip() if (store / name).is_file() else None
    snap["systemd_user_units"] = sorted(str(x.relative_to(p["home"])) for x in p["home"].glob(".config/systemd/user/**/*"))
    return snap


def active_key(snap):
    cur = snap["current"]
    return Path(cur).name if cur else None


def versions_of(snap):
    # versions/nightly is the legacy running-manager copy that nightly activations
    # promote on purpose; only immutable release keys are compared for reuse.
    return sorted({k.split("/")[1] for k in snap["files"] if k.startswith("versions/") and k.count("/") >= 2} - {"nightly"})


def probe_versions(p, env):
    out = {}
    for name in CLI_ALIASES + HAND_ALIASES:
        path = p["store"] / "bin" / name
        if not os.path.lexists(path):
            out[name] = {"present": False}
            continue
        r = run(f"version-{name}", [str(path), "--version"], env, timeout=30)
        text = r["out"] + r["err"]
        out[name] = {"present": True, "link": os.readlink(path) if path.is_symlink() else None,
                     "realpath": os.path.realpath(path), "exit": r["exit"],
                     "commit": re.findall(r"Commit SHA: ([0-9a-f]{40})", text),
                     "identity": re.findall(r"Hand Identity: (\S+)", text), "first_line": text.strip().splitlines()[:1]}
    return out


def verify_installed(p, snap, sha, label):
    """Installed bytes equal the independently streamed, manifest-checked payloads."""
    key = expected_key(sha)
    v = verify_release(sha)
    for logical, a in v["assets"].items():
        check(f"{label}: {a['asset']} digest matches SHA256SUMS", a["archive_sha256"] == a["manifest_sha256"],
              asset=a["asset"], streamed=a["archive_sha256"], manifest=a["manifest_sha256"])
    vdir = p["store"] / "versions" / key
    pairs = [("nanocodex", CLI_ASSET), ("nanocodex2", HAND_ASSET), ("nanocodex-vm-guest", GUEST_ASSET)]
    for fname, logical in pairs:
        path = vdir / fname
        ok = path.exists() and file_sha(path.resolve()) == v["assets"][logical]["raw_sha256"]
        check(f"{label}: versions/{key}/{fname} equals the published {v['assets'][logical]['asset']} payload", ok,
              link=os.readlink(path) if path.is_symlink() else None,
              installed=file_sha(path.resolve()) if path.exists() else None, published=v["assets"][logical]["raw_sha256"])
        receipt = vdir / f"{fname}.sha256"
        if receipt.exists():
            check(f"{label}: {fname}.sha256 receipt equals the published payload digest",
                  receipt.read_text().strip() == v["assets"][logical]["raw_sha256"], receipt=receipt.read_text().strip())
    # Voice runtime: archive receipt equals the manifest, and every published member is present and intact.
    voice = v["assets"][VOICE_ASSET]
    arc = vdir / "nanocodex-voice.archive.sha256"
    check(f"{label}: voice archive receipt equals SHA256SUMS", arc.is_file() and arc.read_text().strip() == voice["manifest_sha256"],
          receipt=arc.read_text().strip() if arc.is_file() else None, manifest=voice["manifest_sha256"])
    bad = {}
    for name, digest in voice["members"].items():
        f = vdir / name
        got = file_sha(f) if f.is_file() and not f.is_symlink() else None
        if got != digest:
            bad[name] = {"expected": digest, "installed": got}
    check(f"{label}: all {len(voice['members'])} published voice files present and byte-identical", voice["members"] and not bad,
          mismatched=bad, sample=sorted(voice["members"])[:5])
    rec = vdir / "nanocodex-voice.sha256"
    if rec.is_file():
        lines = dict(reversed(l.split("  ", 1)) for l in rec.read_text().splitlines() if "  " in l)
        check(f"{label}: voice receipt lists exactly the published members", set(lines) == set(voice["members"]) and
              all(voice["members"][k] == lines[k] for k in lines), receipt_entries=len(lines))
    return key


def check_entrypoints(p, probes, sha, label, unified_expected=True, allow_hand_revision=False):
    for name in CLI_ALIASES:
        pr = probes.get(name, {})
        check(f"{label}: bin/{name} prints exactly one Commit SHA {sha[:12]}", pr.get("present") and pr.get("exit") == 0 and pr.get("commit") == [sha],
              observed=pr)
        if unified_expected:
            check(f"{label}: bin/{name} links ../current/nanocodex", pr.get("link") == "../current/nanocodex", link=pr.get("link"))
    active = p["store"] / "current" / "hand-identity"
    # A version activated by a pre-identity updater has no hand-identity file; the
    # unified CLI still reports the identity of the Hand built with it.
    ident = active.read_text().strip() if active.is_file() else next(iter(probes.get("nanocodex", {}).get("identity") or []), None)
    for name in HAND_ALIASES:
        pr = probes.get(name, {})
        # The nightly Hand records no commit (its identity is the reuse key), so the
        # alias must run the Hand and report the active version's Hand Identity.
        provenance = bool(ident) and pr.get("identity") == [ident]
        if allow_hand_revision and not ident:
            provenance = pr.get("commit") == [sha] and not pr.get("identity")
        check(f"{label}: bin/{name} links the selected Hand and reports the expected identity or candidate revision",
              pr.get("present") and pr.get("exit") == 0 and pr.get("link") == "../current/nanocodex2"
              and (pr.get("first_line") or [""])[0].startswith("nanocodex-hand Version:") and provenance,
              observed=pr, active_identity=ident)


def manager_copies_unchanged(before, after, label):
    """A repeat no-op update must not rewrite the full-size manager copies either."""
    unchanged_files(before, after, [], f"{label}: no-op leaves manager copies (versions/nightly, updater/)", ("versions/nightly/", "updater/"))


def legacy_activation_entrypoints(p, probes, sha, label):
    """The OLD (pre-unified) updater activated NEW: it only knows bin/nanocodex and a
    bin/nanocodex2 that runs current/nanocodex2 (now the Hand daemon)."""
    pr = probes.get("nanocodex", {})
    check(f"{label}: bin/nanocodex runs NEW (one Commit SHA {sha[:12]})", pr.get("exit") == 0 and pr.get("commit") == [sha], observed=pr)
    n2 = probes.get("nanocodex2", {})
    missing = [n for n in ("nc", "ncl", *HAND_ALIASES) if not probes.get(n, {}).get("present")]
    check(f"{label}: legacy-updater activation leaves bin/nanocodex2 linked to the CLI and every NEW alias present",
          n2.get("commit") == [sha] and n2.get("link") == "../current/nanocodex" and not missing,
          nanocodex2_link=n2.get("link"), nanocodex2_version_from=(n2.get("first_line") or [None])[0], missing_aliases=missing,
          note="the OLD updater links bin/nanocodex2 -> ../current/nanocodex2 (the Hand); --version/--help are the Hand's")
    # The Hand forwards user commands to the CLI, so managed commands through the
    # stale link must keep working (actual run, not inferred from --version).
    managed_status(p["store"] / "bin/nanocodex2", base_env(p), f"{label}: stale bin/nanocodex2 forwards")


def check_old_aliases(probes, label):
    """OLD predates the unified CLI: bin/nanocodex is the local tree and
    bin/nanocodex2 the managed CLI. Every present entrypoint must run OLD."""
    # Only OLD's own entrypoints are asserted; NEW-only aliases are unsupported after a legacy rollback.
    present = {n: pr for n, pr in probes.items() if pr.get("present") and n in ("nanocodex", "nanocodex2")}
    check(f"{label}: bin/nanocodex and bin/nanocodex2 are present", {"nanocodex", "nanocodex2"} <= set(present), present=sorted(present))
    for name, pr in present.items():
        check(f"{label}: bin/{name} runs OLD (one Commit SHA {OLD[:12]})", pr.get("exit") == 0 and pr.get("commit") == [OLD],
              link=pr.get("link"), commit=pr.get("commit"), first_line=pr.get("first_line"), exit=pr.get("exit"))


def record_step(name, p, extra=None):
    snap = snapshot(p)
    state["steps"][name] = {"snapshot": snap, **(extra or {})}
    save()
    return snap


def unchanged_files(before, after, keys, label, prefixes=()):
    """Prove a cached activation rewrote none of the version/Hand files."""
    changed = {}
    for k, v in before["files"].items():
        if any(k.startswith(f"versions/{key}/") for key in keys) or k.startswith(("hand-versions/", *prefixes)):
            if after["files"].get(k) != v:
                changed[k] = {"before": v, "after": after["files"].get(k)}
    check(f"{label}: cached version and Hand files are unchanged (same inode, size, mtime and SHA-256)", not changed,
          compared=sum(1 for k in before["files"] if k.startswith(("versions/", "hand-versions/"))), changed=changed)


def installer(p, sha, name):
    """Run exactly the public installer bytes fetched for this step. It re-executes
    the installer from refs/tags/nightly-SHA (immutable), recorded alongside."""
    tag = f"nightly-{sha}"
    env = dict(base_env(p), NANOCODEX_RELEASE_TAG=tag)
    d = ART / "installers"; d.mkdir(exist_ok=True)
    script = d / f"{counter[0] + 1:03d}-{cur_step[0]}-public-install.sh"
    with http_get(INSTALLER_URL) as r:
        script.write_bytes(r.read())
    tagged = d / f"{tag}-install.sh"
    if not tagged.exists():
        with http_get(f"https://raw.githubusercontent.com/{REPO}/refs/tags/{tag}/install") as r:
            tagged.write_bytes(r.read())
    digests = {"public": hashlib.sha256(script.read_bytes()).hexdigest(), "tagged": hashlib.sha256(tagged.read_bytes()).hexdigest()}
    state.setdefault("installers", {})[script.name] = {"url": INSTALLER_URL, "tagged_url": f"refs/tags/{tag}/install", **digests}; save()
    log(f"  public installer {script.name} sha256 {digests['public']}; tagged {tag} installer sha256 {digests['tagged']}")
    return run(name, ["bash", str(script), "--no-setup", "--no-modify-path"], env, cwd=p["ws"])


def no_service_side_effects(p, snap, label):
    check(f"{label}: no pending update, transaction journal or user units", snap["pending-update"] is None and
          snap["update-transaction.json"] is None and not snap["systemd_user_units"],
          pending=snap["pending-update"], units=snap["systemd_user_units"])


# --- steps
def step_a1():
    p = prefix_paths("a")
    r = installer(p, OLD, "installer-old")
    require("installer for nightly-OLD exits 0", r["exit"] == 0, stderr=r["err"][-2000:])
    snap = record_step("a1", p)
    key = verify_installed(p, snap, OLD, "OLD first install")
    check("OLD install activates its immutable nightly key", active_key(snap) == key, current=snap["current"], expected=key)
    probes = probe_versions(p, base_env(p))
    state["steps"]["a1"]["probes"] = probes; save()
    check_old_aliases(probes, "OLD first install")
    no_service_side_effects(p, snap, "OLD install")
    hs = run("hand-status", [str(p["store"] / "bin/nanocodex"), "hand", "status"], base_env(p), timeout=60)
    check("namespace: hand status reports no Linux Hand owner", hs["exit"] == 0 and '"installed": false' in hs["out"], out=hs["out"], err=hs["err"][-500:])


def pointer_names_new(sha=None):
    sha = sha or NEW
    ptr = release("nightly", refresh=True)
    require(f"nightly pointer release targets {sha[:12]}", ptr.get("target_commitish") == sha, target=ptr.get("target_commitish"))
    imm = release(f"nightly-{sha}", refresh=True)
    require(f"immutable nightly-{sha[:12]} exists and targets it", imm.get("target_commitish") == sha and imm.get("tag_name") == f"nightly-{sha}")


def step_a2():
    p = prefix_paths("a")
    pointer_names_new()
    before = snapshot(p)
    old_key = expected_key(OLD)
    require("prefix A has OLD active before update --nightly", active_key(before) == old_key, current=before["current"])
    r = run("update-nightly-by-old", [str(p["store"] / "bin/nanocodex"), "update", "--nightly"], base_env(p))
    require("OLD CLI update --nightly exits 0", r["exit"] == 0, out=r["out"][-1500:], err=r["err"][-1500:])
    snap = record_step("a2", p, {"out": r["out"], "err": r["err"]})
    key = verify_installed(p, snap, NEW, "NEW via OLD update --nightly")
    check("update --nightly activates the NEW immutable nightly key", active_key(snap) == key, current=snap["current"], expected=key)
    check("update --nightly reports the activation with the previous version", key in r["out"] + r["err"] and old_key in r["out"] + r["err"], out=r["out"][-800:])
    unchanged_files(before, snap, [old_key], "OLD retained for rollback")
    probes = probe_versions(p, base_env(p)); state["steps"]["a2"]["probes"] = probes; save()
    legacy_activation_entrypoints(p, probes, NEW, "NEW (activated by OLD updater)")
    no_service_side_effects(p, snap, "NEW via update")


def step_a3():
    p = prefix_paths("a")
    before = snapshot(p)
    r = run("update-nightly-by-new", [str(p["store"] / "bin/nanocodex"), "update", "--nightly"], base_env(p))
    require("NEW CLI update --nightly (already current) exits 0", r["exit"] == 0, err=r["err"][-1500:])
    snap = record_step("a3", p, {"out": r["out"], "err": r["err"]})
    check("repeat update keeps NEW active", active_key(snap) == expected_key(NEW), current=snap["current"])
    unchanged_files(before, snap, versions_of(before), "repeat update --nightly")
    manager_copies_unchanged(before, snap, "repeat update --nightly")
    check("repeat update created no additional version", versions_of(before) == versions_of(snap), versions=versions_of(snap))
    probes = probe_versions(p, base_env(p)); state["steps"]["a3"]["probes"] = probes; save()
    check_entrypoints(p, probes, NEW, "NEW after its own update --nightly")


def rollback(p, label, name):
    before = snapshot(p)
    r = installer(p, OLD, name)
    require(f"{label}: installer rollback to nightly-OLD exits 0", r["exit"] == 0, err=r["err"][-2000:])
    snap = snapshot(p)
    old_key, new_key = expected_key(OLD), expected_key(NEW)
    check(f"{label}: rollback activates the OLD key", active_key(snap) == old_key, current=snap["current"])
    if old_key in versions_of(before):
        unchanged_files(before, snap, [old_key, new_key], f"{label}: cached rollback")
    else:
        unchanged_files(before, snap, [new_key], f"{label}: rollback keeps NEW")
        verify_installed(p, snap, OLD, f"{label}: OLD")
    probes = probe_versions(p, base_env(p))
    check_old_aliases(probes, f"{label}: after rollback")
    # OLD predates the unified CLI; its legacy installer cannot retire NEW-only
    # aliases. Record what they now run; they are unsupported after this rollback.
    residual = {n: {"link": pr.get("link"), "first_line": pr.get("first_line"), "commit": pr.get("commit")}
                for n, pr in probes.items() if n not in ("nanocodex", "nanocodex2") and pr.get("present")}
    state.setdefault("legacy_rollback_residual_aliases", {})[label] = residual; save()
    log(f"  {label}: NEW-only aliases left by the legacy OLD installer (unsupported): {json.dumps(residual)}")
    modes(p, f"{label} OLD roles", OLD)
    no_service_side_effects(p, snap, label)
    return before, snap, r, probes


def step_a4():
    p = prefix_paths("a")
    before, snap, r, probes = rollback(p, "prefix A", "installer-rollback")
    state["steps"]["a4"] = {"snapshot": snap, "out": r["out"], "err": r["err"], "probes": probes}; save()


def roll_forward(p, label, step):
    before = snapshot(p)
    r = run("update-nightly-forward", [str(p["store"] / "bin/nanocodex"), "update", "--nightly"], base_env(p))
    require(f"{label}: OLD CLI update --nightly back to NEW exits 0", r["exit"] == 0, err=r["err"][-1500:])
    snap = snapshot(p)
    check(f"{label}: cached NEW reactivated", active_key(snap) == expected_key(NEW), current=snap["current"])
    check(f"{label}: no new version downloaded", versions_of(before) == versions_of(snap), versions=versions_of(snap))
    unchanged_files(before, snap, versions_of(before), f"{label}: cached roll-forward")
    probes = probe_versions(p, base_env(p))
    legacy_activation_entrypoints(p, probes, NEW, f"{label}: NEW after roll-forward by OLD updater")
    state["steps"][step] = {"snapshot": snap, "out": r["out"], "err": r["err"], "probes": probes}; save()
    return snap


def step_a5():
    roll_forward(prefix_paths("a"), "prefix A", "a5")


def published_identity(sha):
    """The Hand identity a release publishes beside its Linux Hand (absent before identities)."""
    tag = f"nightly-{sha}"
    rel, manifest = release(tag), sums(tag)
    a = next((a for a in rel["assets"] if a["name"] == f"{HAND_ASSET}.identity"), None)
    if not a:
        return None
    with http_get(a["browser_download_url"]) as r:
        body = r.read()
    listed = manifest.get(a["name"])
    check(f"{tag}: published {a['name']} is covered by SHA256SUMS", listed is None or listed == hashlib.sha256(body).hexdigest(),
          listed=listed, digest=hashlib.sha256(body).hexdigest())
    fields = body.decode().split()
    return fields[0] if fields else None


def reuse_key(identity, signing="unsigned"):
    """scripts/release/hand-identity.sh key for a Linux Hand (no packaging inputs)."""
    return hashlib.sha256(f"hand-identity {identity}\ntarget {TRIPLE}\nsigning {signing}\n".encode()).hexdigest()


def identity_layout(p, snap, label, sha=None, key=None):
    sha = sha or NEW
    key = key or expected_key(sha)
    ident_file = p["store"] / "versions" / key / "hand-identity"
    ident = ident_file.read_text().strip() if ident_file.is_file() else None
    link = snap["files"].get(f"versions/{key}/nanocodex2", {})
    stored = snap["files"].get(f"hand-versions/{ident}/nanocodex2", {}) if ident else {}
    check(f"{label}: Hand of {key} stored once under hand-versions/<identity> and linked from the version",
          bool(ident) and link.get("type") == "link" and link.get("target") == f"../../hand-versions/{ident}/nanocodex2"
          and stored.get("type") == "file", identity=ident, link=link, stored=stored,
          hand_versions=sorted({k.split('/')[1] for k in snap['files'] if k.startswith('hand-versions/')}))
    return ident


def step_b1():
    p = prefix_paths("b")
    pointer_names_new()
    r = installer(p, NEW, "installer-new")
    require("installer for nightly-NEW exits 0", r["exit"] == 0, err=r["err"][-2000:])
    snap = record_step("b1", p, {"out": r["out"], "err": r["err"]})
    key = verify_installed(p, snap, NEW, "NEW fresh install")
    check("fresh NEW install activates its immutable key", active_key(snap) == key, current=snap["current"])
    ident = identity_layout(p, snap, "NEW fresh install")
    pub = published_identity(NEW)
    check("NEW fresh install: published Hand reuse key is derived from the stored Hand Identity", bool(ident) and reuse_key(ident) == pub,
          stored_identity=ident, derived_key=reuse_key(ident) if ident else None, published_key=pub)
    stored = snap["files"].get(f"hand-versions/{ident}/nanocodex2", {})
    check("NEW fresh install: stored identity Hand equals the published Hand payload",
          stored.get("sha256") == verify_release(NEW)["assets"][HAND_ASSET]["raw_sha256"], stored=stored.get("sha256"))
    probes = probe_versions(p, base_env(p)); state["steps"]["b1"]["probes"] = probes; save()
    check_entrypoints(p, probes, NEW, "NEW fresh install")
    for n in CLI_ALIASES + HAND_ALIASES:
        check(f"NEW fresh install: bin/{n} reports Hand Identity {ident}", probes.get(n, {}).get("identity") == [ident], observed=probes.get(n, {}).get("identity"))
    no_service_side_effects(p, snap, "NEW fresh install")


def step_b2():
    p = prefix_paths("b")
    before, snap, r, probes = rollback(p, "prefix B", "installer-rollback")
    identity_layout(p, snap, "prefix B after rollback")
    state["steps"]["b2"] = {"snapshot": snap, "out": r["out"], "err": r["err"], "probes": probes}; save()


def step_b3():
    p = prefix_paths("b")
    snap = roll_forward(p, "prefix B", "b3")
    identity_layout(p, snap, "prefix B after roll-forward")
    before = snap
    r = run("update-nightly-by-new", [str(p["store"] / "bin/nanocodex"), "update", "--nightly"], base_env(p))
    require("prefix B: NEW CLI update --nightly (current) exits 0", r["exit"] == 0, err=r["err"][-1500:])
    after = snapshot(p)
    unchanged_files(before, after, versions_of(before), "prefix B: repeat update by NEW")
    manager_copies_unchanged(before, after, "prefix B: repeat update by NEW")
    check_entrypoints(p, probe_versions(p, base_env(p)), NEW, "prefix B: NEW's own activation repairs entrypoints")
    state["steps"]["b3"]["repeat"] = {"snapshot": after, "out": r["out"], "err": r["err"]}; save()


# --- mode starts
def responses_server():
    requests = []

    def sse(text):
        ev = [{"type": "response.created", "response": {"id": "r"}},
              {"type": "response.output_item.done", "item": {"type": "message", "role": "assistant", "id": f"m{len(requests)}",
               "content": [{"type": "output_text", "text": text}]}},
              {"type": "response.completed", "response": {"id": "r", "usage": {"input_tokens": 1, "input_tokens_details": None,
               "output_tokens": 1, "output_tokens_details": None, "total_tokens": 2}}}]
        return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in ev).encode()

    class H(BaseHTTPRequestHandler):
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))) or b"{}")
            requests.append({"path": self.path, "body": body})
            last = ""
            for item in reversed(body.get("input", [])):
                if isinstance(item, dict) and item.get("role") == "user":
                    content = item.get("content", [])
                    last = content if isinstance(content, str) else " ".join(
                        c.get("text", "") for c in content if isinstance(c, dict))
                    break
            word = next((w for w in re.findall(r"[A-Z]+_[A-Z_]+", last)), "UNKNOWN")
            payload = sse("ANSWER_" + word)
            self.send_response(200); self.send_header("content-type", "text/event-stream")
            self.send_header("content-length", str(len(payload))); self.end_headers(); self.wfile.write(payload)

        def log_message(self, *a):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), H)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, f"http://127.0.0.1:{server.server_address[1]}/v1", requests


def pty_session(argv, env, cwd, transcript, until, timeout=40, send=None):
    """Run argv on a PTY; answer cursor queries; call until(text, child) -> bool."""
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
    child = subprocess.Popen(argv, cwd=cwd, env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
    os.close(slave)
    buf, reached, deadline = bytearray(), False, time.monotonic() + timeout
    try:
        while time.monotonic() < deadline:
            if select.select([master], [], [], 0.1)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError as e:
                    if e.errno == errno.EIO:
                        break
                    raise
                if not chunk:
                    break
                buf.extend(chunk)
                if b"\x1b[6n" in chunk:
                    os.write(master, b"\x1b[1;1R")
            plain = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07", "", buf.decode("utf-8", "replace"))
            res = until(plain, child, lambda b: os.write(master, b))
            if res:
                reached = res
                break
            if child.poll() is not None:
                # Drain final output.
                time.sleep(0.3)
                while select.select([master], [], [], 0.1)[0]:
                    try:
                        c = os.read(master, 65536)
                    except OSError:
                        break
                    if not c:
                        break
                    buf.extend(c)
                break
    finally:
        if child.poll() is None:
            try:
                os.write(master, b"\x03"); time.sleep(0.3); os.write(master, b"\x03")
            except OSError:
                pass
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL); child.wait()
        os.close(master)
        plain = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07", "", buf.decode("utf-8", "replace"))
        transcript.write_text(plain)
    return reached, child.returncode, plain


def registrations(home):
    regs = []
    for f in home.glob("**/tui/instances/*.json"):
        try:
            regs.append(json.loads(f.read_text()))
        except (ValueError, OSError):
            pass
    return regs


def managed_status(managed, env, label):
    st = run("managed-status", [str(managed), "status"], env, timeout=60)
    state.setdefault("managed", {}).setdefault(label, {})["status"] = {"exit": st["exit"], "out": st["out"][-1000:], "err": st["err"][-1000:]}; save()
    try:
        authenticated = json.loads(st["out"]).get("authenticated")
    except ValueError:
        authenticated = None
    check(f"{label}: managed status exits 0 and reports authenticated=false with the empty HOME",
          st["exit"] == 0 and authenticated is False, exit=st["exit"], out=st["out"][-300:], err=st["err"][-300:])
    return st


def modes(p, label, sha):
    env = base_env(p)
    store = p["store"]
    server, url, reqs = responses_server()
    common = ["--api-key", "synthetic-test-key", "--api-base-url", url, "--responses-transport", "https", "--browser=none",
              "--mcp-defaults", "false", "--web-search", "false", "--image-generation", "false"]
    # Select the local and managed CLIs from what the installed bin/nanocodex offers, not
    # from which release it is: a pre-unified release ships the local tree as bin/nanocodex
    # (its "run" takes --api-key) and the managed CLI as bin/nanocodex2; a unified release
    # serves the managed tree as bin/nanocodex and the local tree as bin/ncl.
    layout = run("nanocodex-run-help", [str(store / "bin/nanocodex"), "run", "--help"], env, timeout=30)
    unified = not (layout["exit"] == 0 and "--api-key" in layout["out"])
    check(f"{label}: bin/nanocodex run --help identifies the CLI layout", layout["exit"] == 0,
          layout="unified" if unified else "pre-unified", head=layout["out"].splitlines()[:1], err=layout["err"][-400:])
    if sha != OLD:
        check(f"{label}: the release ships the unified CLI (bin/nanocodex run is the managed tree)", unified)
    ncl = store / ("bin/ncl" if unified else "bin/nanocodex")
    managed = store / ("bin/nanocodex" if unified else "bin/nanocodex2")
    if unified:
        check(f"{label}: ncl and nanocodex are the same executable", os.path.realpath(ncl) == os.path.realpath(managed),
              ncl=os.path.realpath(ncl), nanocodex=os.path.realpath(managed))
    hl = run("local-help", [str(ncl), "--help"], env, timeout=30)
    hm = run("managed-help", [str(managed), "--help"], env, timeout=30)
    check(f"{label}: argv[0] selects different command trees", hl["exit"] == 0 and hm["exit"] == 0 and hl["out"] != hm["out"],
          ncl_head=hl["out"].splitlines()[:3], nanocodex_head=hm["out"].splitlines()[:3])
    n0 = len(reqs)
    r = run("ncl-run", [str(ncl), "run", *common, "--cwd", str(p["ws"]), "LOCAL_RUN_PROMPT"], env, cwd=p["ws"], timeout=120)
    check(f"{label}: local ncl run completes a turn against the synthetic Responses server",
          r["exit"] == 0 and "ANSWER_LOCAL_RUN_PROMPT" in r["out"] and any("LOCAL_RUN_PROMPT" in json.dumps(q["body"]) for q in reqs[n0:]),
          exit=r["exit"], requests=len(reqs) - n0, out_tail=r["out"][-400:], err_tail=r["err"][-600:])
    if not unified:
        managed_status(managed, env, label)
        server.shutdown(); return
    # Local TUI: registration, then one real model turn rendered in the TUI.
    tui_t = ART / f"{label.replace(' ', '-')}-ncl-tui.txt"
    sent = [False]

    def local_until(text, child, write):
        regs = [g for g in registrations(p["home"]) if g.get("pid") == child.pid and g.get("active_session_id")]
        if regs and not sent[0]:
            time.sleep(1.0); write(b"TUI_TURN_PROMPT"); time.sleep(0.4); write(b"\r"); sent[0] = True
        return regs[0] if sent[0] and regs and "ANSWER_TUI_TURN_PROMPT" in text else False

    n0 = len(reqs)
    reg, code, text = pty_session([str(ncl), "--cwd", str(p["ws"]), *common], env, p["ws"], tui_t, local_until, timeout=60)
    check(f"{label}: local TUI starts, registers and renders a synthetic model turn",
          bool(reg) and any("TUI_TURN_PROMPT" in json.dumps(q["body"]) for q in reqs[n0:]),
          backend=(reg or {}).get("backend"), session=(reg or {}).get("active_session_id"), transcript=tui_t.name, exit=code)
    # Managed TUI with an empty HOME: either a managed session or an honest login boundary.
    man_t = ART / f"{label.replace(' ', '-')}-managed-tui.txt"
    boundary = re.compile(r"(log ?in|sign ?in|not (signed|logged) in|authentication|authenticate|authenticating)", re.I)
    first = {}

    def managed_until(text, child, write):
        regs = [g for g in registrations(p["home"]) if g.get("pid") == child.pid]
        if regs:
            return {"registered": {k: v for k, v in regs[0].items() if k != "auth_token"}}
        m = boundary.search(text)
        if not m or "panicked" in text or child.poll() is not None:
            first.clear()
            return False
        # Only a prompt that is still displayed by a live process >= 2 s later, on
        # freshly read output, counts as the login boundary.
        first.setdefault("t", time.monotonic())
        if time.monotonic() - first["t"] >= 2:
            line = next((l.strip() for l in text.splitlines() if boundary.search(l)), m.group(0))
            return {"login_boundary": line, "alive": True}
        return False

    res, code, text = pty_session([str(managed)], env, p["ws"], man_t, managed_until, timeout=45)
    login_error = next((l.strip() for l in text.splitlines() if "No account login for this origin" in l), None)
    if not res and login_error and code not in (0, None) and "panicked" not in text:
        res = {"pre_tui_login_error": login_error, "exit": code}
    state.setdefault("managed", {})[label] = {"result": res, "exit": code, "transcript": man_t.name, "tail": text[-1500:]}; save()
    check(f"{label}: managed start reaches a session, a live login prompt, or the exact no-account-login error (empty HOME)",
          bool(res) and "panicked" not in text, observed=res, exit=code, transcript=man_t.name)
    st = managed_status(managed, env, label)
    hs = run("hand-status", [str(managed), "hand", "status"], env, timeout=60)
    check(f"{label}: hand status reports no Linux Hand owner inside the namespace", hs["exit"] == 0 and '"installed": false' in hs["out"], out=hs["out"])
    nh = run("nc-hand-status", [str(store / "bin/nc-hand"), "status"], env, timeout=60) if (store / "bin/nc-hand").exists() else {"exit": "missing", "out": "", "err": "bin/nc-hand is absent"}
    check(f"{label}: nc-hand status is hand status", nh["exit"] == 0 and nh["out"] == hs["out"], out=nh["out"], err=nh["err"][-400:])
    server.shutdown()


def step_modes(sha=None, tag="NEW"):
    sha = sha or NEW
    for name in ("a", "b"):
        p = prefix_paths(name)
        snap = snapshot(p)
        if active_key(snap) == expected_key(sha):
            modes(p, f"prefix {name.upper()} {tag}", sha)
        else:
            check(f"modes: prefix {name} has {tag} active", False, current=snap["current"])


def hand_entries(snap):
    return sorted({k.split("/")[1] for k in snap["files"] if k.startswith("hand-versions/")})


def step_b4():
    """Canonical Hand reuse: the exact published NEW pair, selected again through the
    public local-pair selector under a different key, must reuse the same
    hand-versions/<identity>/nanocodex2 file (same inode), then update --nightly returns."""
    p = prefix_paths("b")
    before = snapshot(p)
    key = expected_key(NEW)
    require("prefix B has NEW active before the local-pair selection", active_key(before) == key, current=before["current"])
    ident = (p["store"] / "versions" / key / "hand-identity").read_text().strip()
    canonical = f"hand-versions/{ident}/nanocodex2"
    v = verify_release(NEW)
    pair = p["root"] / "published-pair"
    shutil.rmtree(pair, ignore_errors=True); pair.mkdir()
    # Independent copies of the installed, published bytes at a user-chosen path
    # (no hard links, so the canonical file's inode/nlink/ctime stay evidence).
    shutil.copyfile(p["store"] / "versions" / key / "nanocodex", pair / "nanocodex")
    shutil.copyfile(p["store"] / canonical, pair / "nanocodex-hand")
    os.chmod(pair / "nanocodex", 0o755); os.chmod(pair / "nanocodex-hand", 0o755)
    for f, logical in (("nanocodex", CLI_ASSET), ("nanocodex-hand", HAND_ASSET)):
        require(f"published-pair/{f} is the published {logical} payload", file_sha(pair / f) == v["assets"][logical]["raw_sha256"])
    voice = verify_release(NEW)["assets"][VOICE_ASSET]
    with http_get(next(a for a in release(f"nightly-{NEW}")["assets"] if a["name"] == VOICE_ASSET)["browser_download_url"]) as r:
        (pair / VOICE_ASSET).write_bytes(r.read())
    require("downloaded voice archive matches SHA256SUMS", file_sha(pair / VOICE_ASSET) == voice["manifest_sha256"])
    r = run("update-path-published-pair", [str(p["store"] / "bin/nanocodex"), "update", "--path", str(pair / "nanocodex"),
             "--hand-binary", str(pair / "nanocodex-hand"), "--voice-archive", str(pair / VOICE_ASSET)], base_env(p))
    require("update --path with the published pair exits 0", r["exit"] == 0, out=r["out"][-1500:], err=r["err"][-1500:])
    snap = snapshot(p)
    local = active_key(snap)
    check("local-pair selection activates a distinct local key", bool(local) and local.startswith("local-") and local != key,
          current=snap["current"], out=r["out"][-600:], err=r["err"][-800:])
    link = snap["files"].get(f"versions/{local}/nanocodex2", {})
    check("local version links the canonical identity Hand", link.get("type") == "link" and link.get("target") == "../../" + canonical, link=link)
    check("canonical identity Hand file reused: same inode, size, mtime and SHA-256", snap["files"].get(canonical) == before["files"].get(canonical),
          before=before["files"].get(canonical), after=snap["files"].get(canonical))
    check("no second Hand stored", hand_entries(snap) == hand_entries(before), hand_versions=hand_entries(snap))
    lid = p["store"] / "versions" / local / "hand-identity"
    check("local version records the same Hand identity", lid.is_file() and lid.read_text().strip() == ident)
    check("local version CLI equals the published CLI", snap["files"].get(f"versions/{local}/nanocodex", {}).get("sha256") == v["assets"][CLI_ASSET]["raw_sha256"])
    arc = p["store"] / "versions" / local / "nanocodex-voice.archive.sha256"
    bad = [n for n, d in voice["members"].items() if not (p["store"] / "versions" / local / n).is_file() or file_sha(p["store"] / "versions" / local / n) != d]
    check("local version voice runtime equals the published archive", arc.is_file() and arc.read_text().strip() == voice["manifest_sha256"] and not bad, mismatched=bad)
    unchanged_files(before, snap, [key], "local-pair selection keeps NEW")
    probes = probe_versions(p, base_env(p))
    check_entrypoints(p, probes, NEW, "local-pair selection")
    back = run("update-nightly-after-path", [str(p["store"] / "bin/nanocodex"), "update", "--nightly"], base_env(p))
    require("update --nightly after the local selection exits 0", back["exit"] == 0, err=back["err"][-1500:])
    after = snapshot(p)
    check("update --nightly returns to the NEW immutable key", active_key(after) == key, current=after["current"], out=back["out"][-500:], err=back["err"][-800:])
    check("canonical identity Hand still the same file after returning", after["files"].get(canonical) == before["files"].get(canonical))
    unchanged_files(snap, after, [key, local], "return to nightly")
    for f in ("nanocodex", "nanocodex-hand", VOICE_ASSET):
        (pair / f).unlink()
    pair.rmdir()
    state["steps"]["b4"] = {"snapshot": after, "local_key": local, "identity": ident, "out": r["out"], "err": r["err"],
                            "back_out": back["out"], "back_err": back["err"]}; save()


def prior_hand_identity(p, new_key, label):
    """NEW's Hand Identity: its hand-identity sidecar or, when a pre-identity updater
    activated NEW (legacy layout, no sidecar), the identity its own Hand reports."""
    sidecar = p["store"] / "versions" / new_key / "hand-identity"
    if sidecar.is_file():
        return sidecar.read_text().strip(), "hand-identity"
    r = run("prior-hand-version", [str(p["store"] / "versions" / new_key / "nanocodex2"), "--version"], base_env(p), timeout=30)
    found = re.findall(r"Hand Identity: (\S+)", r["out"] + r["err"])
    ok = r["exit"] == 0 and len(found) == 1 and re.fullmatch(r"[0-9a-f]{64}", found[0])
    check(f"{label}: NEW without a hand-identity sidecar: its Hand --version reports exactly one 64-hex Hand Identity",
          ok, exit=r["exit"], identity=found, record=r["record"])
    return (found[0] if ok else None), "--version"


def final_upgrade(name, label):
    require("--final-sha is set", bool(FINAL))
    p = prefix_paths(name)
    pointer_names_new(FINAL)
    before = snapshot(p)
    new_key = expected_key(NEW)
    require(f"{label}: NEW active before the final upgrade", active_key(before) == new_key, current=before["current"])
    r = run("update-nightly-final", [str(p["store"] / "bin/nanocodex"), "update", "--nightly"], base_env(p))
    require(f"{label}: NEW CLI update --nightly to FINAL exits 0", r["exit"] == 0, out=r["out"][-1500:], err=r["err"][-1500:])
    snap = snapshot(p)
    key = verify_installed(p, snap, FINAL, f"{label}: FINAL")
    check(f"{label}: FINAL immutable key active", active_key(snap) == key, current=snap["current"])
    unchanged_files(before, snap, versions_of(before), f"{label}: earlier versions retained")
    ident = identity_layout(p, snap, f"{label}: FINAL", FINAL)
    prior_ident, prior_source = prior_hand_identity(p, new_key, label)
    pub_new, pub_final = published_identity(NEW), published_identity(FINAL)
    check(f"{label}: published NEW and FINAL reuse keys derive from the stored Hand Identities",
          bool(prior_ident and ident) and reuse_key(prior_ident) == pub_new and reuse_key(ident) == pub_final,
          stored=[prior_ident, ident], prior_source=prior_source, published=[pub_new, pub_final])
    legacy_hand = f"versions/{new_key}/nanocodex2"
    if prior_source == "--version":
        # A pre-identity updater stored NEW's Hand as a regular file in its version.
        check(f"{label}: sidecar-less NEW Hand is an unchanged regular file equal to the published NEW Hand",
              before["files"].get(legacy_hand, {}).get("type") == "file"
              and before["files"][legacy_hand].get("sha256") == verify_release(NEW)["assets"][HAND_ASSET]["raw_sha256"]
              and snap["files"].get(legacy_hand) == before["files"].get(legacy_hand),
              before=before["files"].get(legacy_hand), after=snap["files"].get(legacy_hand))
    branch = "reused" if pub_new == pub_final else "new-identity"
    state.setdefault("identity_branch", {})[label] = {"branch": branch, "new": pub_new, "final": pub_final}; save()
    log(f"  {label}: published Hand identity NEW {pub_new} FINAL {pub_final} => branch {branch}")
    if branch == "reused" and f"hand-versions/{ident}/nanocodex2" not in before["files"]:
        # Legacy NEW had no canonical Hand: FINAL adopts the canonical file exactly once.
        stored = snap["files"].get(f"hand-versions/{ident}/nanocodex2", {})
        check(f"{label}: unchanged Hand identity first stores the canonical Hand once beside the legacy NEW Hand",
              prior_source == "--version" and prior_ident == ident and stored.get("type") == "file"
              and stored.get("sha256") == verify_release(FINAL)["assets"][HAND_ASSET]["raw_sha256"]
              and set(hand_entries(snap)) == set(hand_entries(before)) | {ident},
              identity=ident, prior_identity=prior_ident, stored=stored, hand_versions=hand_entries(snap))
    elif branch == "reused":
        canonical = f"hand-versions/{ident}/nanocodex2"
        check(f"{label}: unchanged Hand identity reuses the canonical Hand file (same inode, bytes)",
              snap["files"].get(canonical) == before["files"].get(canonical) and hand_entries(snap) == hand_entries(before),
              identity=ident, before=before["files"].get(canonical), after=snap["files"].get(canonical))
    else:
        stored = snap["files"].get(f"hand-versions/{ident}/nanocodex2", {})
        old_canon = f"hand-versions/{prior_ident}/nanocodex2"
        check(f"{label}: changed identity stores the FINAL Hand once beside the untouched NEW Hand",
              stored.get("sha256") == verify_release(FINAL)["assets"][HAND_ASSET]["raw_sha256"]
              and snap["files"].get(old_canon) == before["files"].get(old_canon)
              and set(hand_entries(snap)) == set(hand_entries(before)) | {ident},
              identity=ident, prior_identity=prior_ident, hand_versions=hand_entries(snap))
    probes = probe_versions(p, base_env(p))
    check_entrypoints(p, probes, FINAL, f"{label}: FINAL")
    no_service_side_effects(p, snap, f"{label}: FINAL")
    state["steps"]["c1" if name == "a" else "c2"] = {"snapshot": snap, "out": r["out"], "err": r["err"], "probes": probes,
                                                     "identity": ident, "prior_identity": prior_ident,
                                                     "prior_identity_source": prior_source}; save()


def candidate_sha():
    out = sh([str(CAND[0]), "--version"]).stdout
    shas = re.findall(r"Commit SHA: ([0-9a-f]{40})", out)
    require("candidate CLI reports exactly one Commit SHA", len(shas) == 1, version=out[:400])
    return shas[0]


def step_p1():
    """Candidate fix: the published OLD updater activates the candidate pair through
    its public --path selector (the same legacy activation as a2); the candidate's
    own first run must leave every entrypoint coherent."""
    require("--candidate is set", bool(CAND))
    p = prefix_paths("p")
    r = installer(p, OLD, "installer-old")
    require("installer for nightly-OLD exits 0", r["exit"] == 0, err=r["err"][-1500:])
    sha = candidate_sha()
    r = run("old-update-path-candidate", [str(p["store"] / "bin/nanocodex"), "update", "--path", str(CAND[0]),
             "--hand-binary", str(CAND[1]), "--voice-archive", str(CAND[2])], base_env(p))
    require("OLD updater --path candidate exits 0", r["exit"] == 0, out=r["out"][-1200:], err=r["err"][-1500:])
    snap = snapshot(p)
    key = active_key(snap)
    check("candidate activated under a local key", bool(key) and key.startswith("local-"), current=snap["current"])
    vdir = p["store"] / "versions" / key
    check("candidate CLI and Hand bytes installed unchanged",
          file_sha(vdir / "nanocodex") == file_sha(CAND[0]) and file_sha((vdir / "nanocodex2").resolve()) == file_sha(CAND[1]))
    links = {n: os.readlink(p["store"] / "bin" / n) if (p["store"] / "bin" / n).is_symlink() else None for n in CLI_ALIASES + HAND_ALIASES}
    state.setdefault("candidate", {})["links_after_old_activation"] = links; save()
    require("candidate starts behind the legacy Hand link", links.get("nanocodex2") == "../current/nanocodex2", links=links)
    log(f"  links after OLD-updater activation, before any candidate run: {json.dumps(links)}")
    import fcntl as _fcntl

    def current_links():
        return {n: os.readlink(p["store"] / "bin" / n) if (p["store"] / "bin" / n).is_symlink() else None for n in CLI_ALIASES + HAND_ALIASES}

    stale = p["store"] / "bin/nanocodex2"
    # 1. Another updater holds the store's update.lock: the first managed command
    #    through the stale link must still finish (repair is non-blocking) and must
    #    leave every link untouched.
    with open(p["store"] / "update.lock", "a+") as lock:
        _fcntl.flock(lock, _fcntl.LOCK_EX | _fcntl.LOCK_NB)
        managed_status(stale, base_env(p), "candidate stale bin/nanocodex2 while update.lock is held")
        check("held update.lock: no entrypoint changed", current_links() == links, links=current_links())
    # 2. A foreign NANOCODEX_DIR must not receive (or redirect) the repair.
    foreign = p["root"] / "foreign-store"
    foreign.mkdir(exist_ok=True)
    before_foreign = sorted(str(x.relative_to(foreign)) for x in foreign.rglob("*"))
    managed_status(stale, dict(base_env(p), NANOCODEX_DIR=str(foreign)), "candidate stale bin/nanocodex2 with a foreign NANOCODEX_DIR")
    after_foreign = sorted(str(x.relative_to(foreign)) for x in foreign.rglob("*"))
    check("foreign NANOCODEX_DIR: no entrypoint written there", not [x for x in after_foreign if x.startswith("bin")],
          before=before_foreign, after=after_foreign)
    state["candidate"]["links_after_foreign"] = current_links(); save()
    # 3. The ordinary next run through the stale link repairs every alias.
    managed_status(stale, base_env(p), "candidate stale bin/nanocodex2 after the lock is released")
    state["candidate"]["links_after_repair"] = current_links(); save()
    probes = probe_versions(p, base_env(p))
    check_entrypoints(p, probes, sha, "candidate after OLD-updater activation and its first run", allow_hand_revision=True)
    state["steps"]["p1"] = {"snapshot": snapshot(p), "key": key, "sha": sha, "probes": probes}; save()


def step_p2():
    """Candidate fix: repeating the same --path selection is a no-op that rewrites
    no version, Hand or manager file."""
    require("--candidate is set", bool(CAND))
    p = prefix_paths("p")
    before = snapshot(p)
    r = run("candidate-update-path-again", [str(p["store"] / "bin/nanocodex"), "update", "--path", str(CAND[0]),
             "--hand-binary", str(CAND[1]), "--voice-archive", str(CAND[2])], base_env(p))
    require("candidate repeat update --path exits 0", r["exit"] == 0, err=r["err"][-1500:])
    after = snapshot(p)
    check("repeat --path keeps the same key", active_key(after) == active_key(before), current=after["current"])
    unchanged_files(before, after, versions_of(before), "candidate repeat --path")
    unchanged_files(before, after, [], "candidate repeat --path: manager copies (versions/nightly, updater/)", ("versions/nightly/", "updater/"))
    check_entrypoints(p, probe_versions(p, base_env(p)), state["steps"]["p1"]["sha"], "candidate after repeat --path", allow_hand_revision=True)


def step_c1():
    final_upgrade("a", "prefix A")


def step_c2():
    final_upgrade("b", "prefix B")


def step_final_modes():
    require("--final-sha is set", bool(FINAL))
    step_modes(FINAL, "FINAL")


def step_old_modes():
    p = prefix_paths("a")
    modes(p, "prefix A OLD", OLD)


def drop_to_real_user():
    """The outer namespace maps the caller to root only to mount the private /run and
    read-only binds. Enter a nested user namespace mapping the real uid/gid back, so
    every installer/CLI runs unprivileged like a real user (no CAP_*, euid != 0)."""
    import ctypes
    uid, gid = int(os.environ["NIGHTLY_E2E_UID"]), int(os.environ["NIGHTLY_E2E_GID"])
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.unshare(0x10000000) != 0:  # CLONE_NEWUSER
        raise OSError(ctypes.get_errno(), "unshare(CLONE_NEWUSER)")
    Path("/proc/self/setgroups").write_text("deny")
    Path("/proc/self/uid_map").write_text(f"{uid} 0 1")
    Path("/proc/self/gid_map").write_text(f"{gid} 0 1")
    # The namespace owner keeps a full capability set until execve as a nonzero
    # uid clears it; re-exec so the journey itself runs without capabilities.
    os.environ["NIGHTLY_E2E_DROPPED"] = "1"
    os.execv(sys.executable, [sys.executable, *sys.argv])


def inner():
    if os.geteuid() == 0 and "NIGHTLY_E2E_UID" in os.environ and "NIGHTLY_E2E_DROPPED" not in os.environ:
        drop_to_real_user()
    expected_uid = int(os.environ["NIGHTLY_E2E_UID"]) if "NIGHTLY_E2E_DROPPED" in os.environ else None
    log(f"==== {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())} steps={STEPS} old={OLD} new={NEW} uid={os.getuid()}")
    cur_step[0] = "namespace"
    ident = sh(["id", "-u"]).stdout.strip()
    cap = next((l.split()[1] for l in Path("/proc/self/status").read_text().splitlines() if l.startswith("CapEff:")), None)
    check("namespace: journey runs as the real unprivileged uid (id -u), no effective capabilities",
          expected_uid is not None and ident == str(expected_uid) and os.geteuid() == expected_uid and int(cap, 16) == 0,
          id_u=ident, expected=expected_uid, cap_eff=cap)
    check("namespace: /run is an empty private tmpfs without systemd", not Path("/run/systemd/system").exists(),
          run=sorted(os.listdir("/run")))
    probe = Path("/opt/nanocodex/.e2e-write-probe")
    try:
        probe.write_text("x"); probe.unlink(); writable = True
    except OSError as e:
        writable = str(e)
    check("namespace: /opt/nanocodex is read-only to the journey", writable is not True, error=writable)
    failed = None
    for s in STEPS:
        cur_step[0] = s
        log(f"---- step {s}")
        try:
            globals()["step_" + s.replace("-", "_")]()
            state["steps"].setdefault(s, {})["completed"] = time.time()
        except Exception as e:  # recorded; later dependent steps are not run
            check("step completed", False, error=f"{type(e).__name__}: {e}")
            failed = s
            break
        save()
    fails = [c for c in state["checks"] if not c["ok"]]
    summary = {"steps": STEPS, "failed_step": failed, "checks": len(state["checks"]), "failures": len(fails),
               "failed": [f"{c['step']}: {c['check']}" for c in fails],
               "identity_branch": state.get("identity_branch"),
               "legacy_rollback_residual_aliases": state.get("legacy_rollback_residual_aliases")}
    (ART / "summary.json").write_text(json.dumps(summary, indent=2))
    print(json.dumps(summary, indent=2))
    sys.exit(1 if fails else 0)


if args.inner:
    inner()
else:
    outer()
