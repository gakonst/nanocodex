#!/usr/bin/env python3
"""Private SSH bootstrap. Credentials arrive exclusively over stdin.

The Rust processes own registration/reconciliation and send systemd READY=1.
This installer owns packages, immutable artifacts, and persistent service units.
"""
import fcntl
import gzip
import hashlib
import json
import os
from pathlib import Path
import platform
import pwd
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request

ROOT = Path("/opt/nanocodex")
STATE = Path("/srv/nanocodex")
FIRMWARE_URL = "https://github.com/libkrun/libkrunfw/releases/download/v5.5.0/libkrunfw-x86_64.tgz"
FIRMWARE_SHA256 = "c169206b01c89fbe134f1728bf4f988702bc7f73b4cf73e6fdece447d6fceca1"


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def digest(path):
    checksum = hashlib.sha256()
    with open(path, "rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            checksum.update(chunk)
    return checksum.hexdigest()


def regular(path):
    if path.is_symlink() or (path.exists() and not path.is_file()):
        raise RuntimeError(f"Refusing unexpected file: {path}")


def atomic(path, data, mode=0o644):
    regular(path)
    if path.exists() and path.read_bytes() == data:
        os.chmod(path, mode)
        return False
    fd, temporary = tempfile.mkstemp(dir=path.parent, prefix=".install-")
    try:
        with os.fdopen(fd, "wb") as file:
            os.fchmod(file.fileno(), mode)
            file.write(data)
            file.flush()
            os.fsync(file.fileno())
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)
    return True


def download(url, checksum, cache):
    target = cache / checksum
    regular(target)
    if target.exists() and digest(target) == checksum:
        return target
    if not url.startswith("https://"):
        raise RuntimeError("Artifact downloads require HTTPS")
    with tempfile.NamedTemporaryFile(dir=cache, delete=False) as file:
        temporary = Path(file.name)
        try:
            request = urllib.request.Request(url, headers={"User-Agent": "nanocodex-hand-setup"})
            with urllib.request.urlopen(request, timeout=60) as response:
                shutil.copyfileobj(response, file)
            file.flush()
            if digest(temporary) != checksum:
                raise RuntimeError("Artifact checksum mismatch")
            os.replace(temporary, target)
        finally:
            temporary.unlink(missing_ok=True)
    return target


def unit(command, factory=False):
    return f"""[Unit]
Description=Nanocodex {'VM factory' if factory else 'Linux Hand'}
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=0

[Service]
Type=notify
NotifyAccess=main
User=nanocodex
Group=nanocodex
{'SupplementaryGroups=kvm' if factory else ''}
WorkingDirectory=/srv/nanocodex/workspace
EnvironmentFile=/opt/nanocodex/account.env
Environment=HOME=/srv/nanocodex
Environment=PATH=/usr/local/bin:/usr/bin:/bin
ExecStart=/opt/nanocodex/current/nanocodex2 {command}
Restart=on-failure
RestartSec=5
TimeoutStartSec=120
TimeoutStopSec=90
KillMode=mixed
UMask=0077
CPUWeight=25

[Install]
WantedBy=multi-user.target
""".encode()


def account_get(config, path):
    request = urllib.request.Request(config["origin"] + path,
        headers={"Authorization": "Bearer " + config["credential"], "User-Agent": "nanocodex-hand-setup"})
    with urllib.request.urlopen(request, timeout=15) as response:
        return json.load(response)


def main(stage, config):
    if os.geteuid() != 0 or platform.system() != "Linux" or platform.machine() != "x86_64":
        raise RuntimeError("This setup requires x86_64 Linux and sudo")
    if not Path("/run/systemd/system").is_dir():
        raise RuntimeError("systemd must be running")
    if not shutil.which("apt-get"):
        raise RuntimeError("Automatic package setup currently supports Debian and Ubuntu")
    if not re.fullmatch(r"ncx_live_[A-Za-z0-9_-]+", config["credential"]):
        raise RuntimeError("Invalid enrollment credential")
    if not 1 <= config["max_vms"] <= 64 or not 1 <= config["vm_cpus"] <= 8 or not 128 <= config["vm_memory_mib"] <= 262144:
        raise RuntimeError("Invalid VM shape: current host protocol supports 1–64 VMs; bundled firmware supports 1–8 CPUs per VM")
    for directory in [ROOT, STATE]:
        if directory.is_symlink():
            raise RuntimeError(f"Refusing symlinked install directory: {directory}")
    factory = config["factory_name"]
    if not re.fullmatch(r"[a-z0-9](?:[a-z0-9._-]{0,61}[a-z0-9])?", factory):
        raise RuntimeError("Invalid factory name")
    previous_path = ROOT / "installation.json"
    regular(previous_path)
    previous = json.loads(previous_path.read_text()) if previous_path.exists() else {}
    for field in ["owner", "origin", "factory_name"]:
        if field in previous and previous[field] != config[field]:
            raise RuntimeError(f"Existing installation belongs to another {field}; refusing to replace its retained state")
    if previous and previous["native_only"] != config["native_only"]:
        raise RuntimeError("Existing installation mode differs; keep its factory enabled to preserve retained VMs")
    if not config["native_only"]:
        fd = os.open("/dev/kvm", os.O_RDWR | os.O_CLOEXEC)
        try:
            if fcntl.ioctl(fd, 0xAE00, 0) != 12:
                raise RuntimeError("Unsupported KVM API")
        finally:
            os.close(fd)
        # Account for existing reservations on a rerun; never evict active VMs.
        old_memory = previous.get("max_vms", 0) * previous.get("vm_memory_mib", 0)
        extra_memory = config["max_vms"] * config["vm_memory_mib"] - old_memory
        available = int(next(line.split()[1] for line in Path("/proc/meminfo").read_text().splitlines() if line.startswith("MemAvailable:"))) // 1024
        if extra_memory > available - 2048:
            raise RuntimeError("Not enough available RAM for the requested VM pool; lower --max-vms or --vm-memory-mib")
    print("Installing Linux runtime dependencies…", flush=True)
    packages = ["ca-certificates", "xvfb", "openbox", "xterm", "xauth", "fonts-dejavu-core"]
    if not config["native_only"] and not shutil.which("docker"):
        packages.append("docker.io")
    env = dict(os.environ, DEBIAN_FRONTEND="noninteractive", NEEDRESTART_MODE="l")
    missing = [package for package in packages if subprocess.run(
        ["dpkg-query", "-W", "-f=${Status}", package], capture_output=True, text=True
    ).stdout.strip() != "install ok installed"]
    if missing:
        run("apt-get", "update", env=env, stdout=subprocess.DEVNULL)
        run("apt-get", "install", "-y", "--no-install-recommends", *missing, env=env, stdout=subprocess.DEVNULL)
    try:
        user = pwd.getpwnam("nanocodex")
    except KeyError:
        run("useradd", "--system", "--create-home", "--home-dir", str(STATE), "--shell", "/bin/bash", "nanocodex")
        user = pwd.getpwnam("nanocodex")
    if user.pw_dir != str(STATE):
        raise RuntimeError("Existing nanocodex user has a different home directory")
    STATE.mkdir(exist_ok=True)
    os.chown(STATE, user.pw_uid, user.pw_gid)
    os.chmod(STATE, 0o700)
    for child in ["workspace", "native-state", "factory-state", "cache"]:
        path = STATE / child
        if path.is_symlink():
            raise RuntimeError(f"Refusing symlinked state directory: {path}")
        path.mkdir(exist_ok=True, mode=0o700)
        os.chown(path, user.pw_uid, user.pw_gid)
        os.chmod(path, 0o700)
    for child in ["cache", "releases", "firmware", "images"]:
        (ROOT / child).mkdir(parents=True, exist_ok=True)
    cache = ROOT / "cache"
    artifacts = []
    for artifact in config["artifacts"]:
        if artifact["name"] not in ["nanocodex2", "nanocodex-vm-guest"]:
            raise RuntimeError("Unexpected artifact")
        source = stage / artifact["name"] if artifact.get("local") else download(artifact["url"], artifact["sha256"], cache)
        if digest(source) != artifact["sha256"]:
            raise RuntimeError("Uploaded artifact checksum mismatch")
        content = gzip.decompress(source.read_bytes()) if artifact.get("gzip") else source.read_bytes()
        # Both host and guest must be little-endian x86_64 ELF, not a Mac build.
        if content[:6] != b"\x7fELF\x02\x01" or content[18:20] != b"\x3e\x00":
            raise RuntimeError("Artifact is not an x86_64 Linux executable")
        artifacts.append((artifact["name"], content))
    revision = hashlib.sha256(b"".join(hashlib.sha256(data).digest() for _, data in artifacts)).hexdigest()[:24]
    release = ROOT / "releases" / revision
    release.mkdir(exist_ok=True)
    for name, data in artifacts:
        atomic(release / name, data, 0o755)
    run(str(release / "nanocodex2"), "--version")
    if not config["native_only"]:
        # ELF headers cannot detect a mismatched libc startup object. Exercise
        # the actual guest protocol before activating either service.
        probe = run("runuser", "-u", "nanocodex", "--", str(release / "nanocodex-vm-guest"),
            str(STATE / "workspace"), input=b'{"kind":"ready","payload":{"id":1}}\n',
            capture_output=True, timeout=30)
        if json.loads(probe.stdout) != {"kind": "ready", "payload": {"id": 1, "error": None}}:
            raise RuntimeError("Guest runtime did not answer its readiness protocol")
    if not config["native_only"]:
        firmware = download(FIRMWARE_URL, FIRMWARE_SHA256, cache)
        with tarfile.open(firmware) as archive:
            # Extract only the known ordinary library, never archive paths/links.
            member = archive.getmember("lib64/libkrunfw.so.5.5.0")
            if not member.isfile():
                raise RuntimeError("Invalid firmware archive")
            atomic(ROOT / "firmware" / "libkrunfw.so.5", archive.extractfile(member).read())
        template_key = hashlib.sha256(b"".join((stage / name).read_bytes() for name in ["Dockerfile", "Dockerfile.ext4", "populate-ext4.sh", "build-root.sh"])).hexdigest()[:16]
        template = ROOT / "images" / f"desktop-{template_key}.ext4"
        if not template.exists():
            print("Preparing retained VM desktop template…", flush=True)
            run("docker", "info", stdout=subprocess.DEVNULL)
            run("docker", "build", "-t", f"nanocodex-vm:{template_key}", str(stage))
            run("bash", str(stage / "build-root.sh"), f"nanocodex-vm:{template_key}", str(template), "16384")
        config["template"] = str(template)
    # Establish ownership before enrollment. A failed first install must not let
    # a later invocation attach its retained workspace to a different account.
    public = {key: value for key, value in config.items() if key != "credential"}
    public["revision"] = revision
    atomic(previous_path, (json.dumps(public, indent=2) + "\n").encode())
    # The account key is readable by the system service manager only.
    secret_changed = atomic(ROOT / "account.env", ("NANOCODEX_API_KEY=" + config["credential"] + "\nNANOCODEX_MANAGED_URL=" + config["origin"] + "\n").encode(), 0o600)
    current = ROOT / "current"
    if current.exists() and not current.is_symlink():
        raise RuntimeError("Expected an installation symlink at /opt/nanocodex/current")
    binary_changed = not current.is_symlink() or current.resolve() != release
    replacement = ROOT / "current.next"
    replacement.unlink(missing_ok=True)
    replacement.symlink_to(release)
    os.replace(replacement, current)
    native_command = f"native-hand --workspace /srv/nanocodex/workspace --state-dir /srv/nanocodex/native-state --machine-name {factory} --log-format json"
    units = {"nanocodex-hand.service": unit(native_command)}
    if not config["native_only"]:
        command = f"host --scope user --factory-name {factory} --state-dir /srv/nanocodex/factory-state --vm-template {config['template']} --vm-guest-runtime /opt/nanocodex/current/nanocodex-vm-guest --vm-firmware /opt/nanocodex/firmware --vm-cache /srv/nanocodex/cache --vm-workspace /workspace --max-vms {int(config['max_vms'])} --vm-cpus {int(config['vm_cpus'])} --vm-memory-mib {int(config['vm_memory_mib'])} --log-format json"
        units["nanocodex-factory.service"] = unit(command, factory=True)
    changed = {name: atomic(Path("/etc/systemd/system") / name, contents) for name, contents in units.items()}
    run("systemctl", "daemon-reload")
    for name in units:
        run("systemctl", "enable", name)
        run("systemctl", "restart" if binary_changed or secret_changed or changed[name] else "start", name)
    identity = json.loads((STATE / "native-state" / "identity.json").read_text())
    print("Checking the account Hand and screen catalog…", flush=True)
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        hands = account_get(config, "/v1/account/hands").get("data", [])
        screens = account_get(config, "/v1/account/hands/screens").get("surfaces", [])
        hand = next((hand for hand in hands if hand["id"] == identity["machine_id"]), None)
        if hand and any(screen.get("machine_id") == identity["machine_id"] for screen in screens):
            break
        time.sleep(1)
    else:
        raise RuntimeError("Services started but the Hand and desktop did not appear in the account catalog")
    public["machine_id"] = identity["machine_id"]
    atomic(previous_path, (json.dumps(public, indent=2) + "\n").encode())
    print(json.dumps({"status": "ready", "machine_id": identity["machine_id"], "workspace": hand["workspace"],
        "factory": None if config["native_only"] else factory, "max_vms": 0 if config["native_only"] else config["max_vms"],
        "vm_cpus": config["vm_cpus"], "vm_memory_mib": config["vm_memory_mib"], "revision": revision}))


if __name__ == "__main__":
    try:
        lock = os.open("/run/lock/nanocodex-hand-setup.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        main(Path(sys.argv[1]), json.load(sys.stdin))
    except Exception as error:
        # Never echo config, HTTP bodies, stdin, or credential environment.
        print(f"Hand setup failed: {error}", file=sys.stderr)
        sys.exit(1)
