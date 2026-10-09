#!/usr/bin/env python3
"""Build the native x86_64/aarch64 Wayland payload consumed by the Rust embedder.

No FFmpeg/compositor is included. Waymote's FFmpeg bridge remains provisioned by
Hand installation. Native builds never install packages or change host state.
"""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request

WAYMOTE_URL = 'https://github.com/rockorager/waymote.git'
WAYMOTE_REV = '69f585e6a1dfb84a3ac8135f1a0536aa9d75f6c5'
GRIM_URL = 'https://gitlab.freedesktop.org/emersion/grim.git'
GRIM_TAG = 'v1.5.0'
GRIM_REV = 'b7a99854e46945db9f50ba8d2417ac42321173d1'
PROTOCOLS_URL = 'https://gitlab.freedesktop.org/wayland/wayland-protocols.git'
PROTOCOLS_REV = 'ee78491a237eaff9389a0ccf8680521d074407d3'
ZIG_VERSION = '0.16.0'
ARCHITECTURES = {
    'x86_64': {'triplet':'x86_64-linux-gnu', 'loader':'ld-linux-x86-64.so.2',
               'cflags':'-O2 -march=x86-64 -mtune=generic',
               'zig_sha256':'70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00'},
    'aarch64': {'triplet':'aarch64-linux-gnu', 'loader':'ld-linux-aarch64.so.1',
                'cflags':'-O2 -march=armv8-a -mtune=generic',
                'zig_sha256':'ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17'},
}
PACKAGES = {'ld-linux-x86-64.so.2':'libc6', 'ld-linux-aarch64.so.1':'libc6', 'libc.so.6':'libc6', 'libm.so.6':'libc6',
            'libpthread.so.0':'libc6', 'librt.so.1':'libc6', 'libdl.so.2':'libc6',
            'libwayland-client.so.0':'libwayland-client0', 'libxkbcommon.so.0':'libxkbcommon0',
            'libffi.so.8':'libffi8', 'libpixman-1.so.0':'libpixman-1-0',
            'libpng16.so.16':'libpng16-16', 'libz.so.1':'zlib1g'}


def run(cmd, cwd=None, env=None):
    print('+', shlex.join(map(str, cmd)), flush=True)
    subprocess.run(list(map(str, cmd)), cwd=cwd, env=env, check=True)


def capture(cmd):
    return subprocess.check_output(list(map(str, cmd)), text=True)


# Upstream forges (notably gitlab.freedesktop.org) intermittently answer 5xx.
# Only transport is retried: every fetched source is still verified against its
# pinned commit or SHA256, and a mismatch fails immediately without a retry.
FETCH_ATTEMPTS = 5


def with_network_retries(description, operation):
    for attempt in range(1, FETCH_ATTEMPTS + 1):
        try:
            return operation()
        except (subprocess.CalledProcessError, OSError) as error:
            if attempt == FETCH_ATTEMPTS:
                raise
            delay = 5 * 2 ** (attempt - 1)
            print(f'{description} failed (attempt {attempt}/{FETCH_ATTEMPTS}: {error}); retrying in {delay}s',
                  file=sys.stderr, flush=True)
            time.sleep(delay)


def checkout(root, name, url, revision, tag=None):
    dest = root / name
    if not dest.exists():
        run(['git', 'init', dest])
        run(['git', '-C', dest, 'remote', 'add', 'origin', url])
        with_network_retries(f'{name}: fetching {tag or revision} from {url}',
                             lambda: run(['git', '-C', dest, 'fetch', '--depth=1', 'origin', tag or revision]))
        run(['git', '-C', dest, 'checkout', '--detach', 'FETCH_HEAD'])
    actual = capture(['git', '-C', dest, 'rev-parse', 'HEAD']).strip()
    if actual != revision:
        raise RuntimeError(f'{name}: expected {revision}, got {actual}')
    return dest


def zig_compiler(root, provided, architecture):
    if provided:
        binary = Path(provided).resolve()
    else:
        binary = root / 'zig' / 'zig'
        if not binary.exists():
            archive = root / 'zig.tar.xz'
            if not archive.exists():
                url = f'https://ziglang.org/download/{ZIG_VERSION}/zig-{architecture}-linux-{ZIG_VERSION}.tar.xz'
                partial = archive.with_suffix('.partial')
                # Publish the archive only after a complete download; a short
                # read raises and is retried instead of leaving a torn file.
                with_network_retries(f'downloading {url}', lambda: urllib.request.urlretrieve(url, partial))
                partial.rename(archive)
            if hashlib.sha256(archive.read_bytes()).hexdigest() != ARCHITECTURES[architecture]['zig_sha256']:
                raise RuntimeError('Zig archive SHA256 mismatch')
            with tarfile.open(archive) as tar:
                # Python 3.10 is the Ubuntu 22.04 release-builder baseline.
                # Explicit extraction is portable and rejects every link/special
                # entry instead of relying on 3.12's filter='data'.
                total = 0
                for index, member in enumerate(tar):
                    relative = PurePosixPath(member.name)
                    total += member.size
                    if index > 30000 or total > 1024 * 1024 * 1024:
                        raise RuntimeError('Zig archive exceeds extraction limits')
                    if relative.is_absolute() or '..' in relative.parts or not relative.parts:
                        raise RuntimeError('Unsafe Zig archive path')
                    target = root / 'compiler' / member.name
                    if member.isdir():
                        target.mkdir(parents=True, exist_ok=True)
                    elif member.isfile():
                        target.parent.mkdir(parents=True, exist_ok=True)
                        with tar.extractfile(member) as source, target.open('wb') as output:
                            shutil.copyfileobj(source, output)
                        target.chmod(member.mode & 0o755)
                    else:
                        raise RuntimeError('Links/special entries are forbidden in Zig archive')
            shutil.move(str(root / 'compiler' / f'zig-{architecture}-linux-{ZIG_VERSION}'), root / 'zig')
    if capture([binary, 'version']).strip() != ZIG_VERSION:
        raise RuntimeError('Zig 0.16.0 is required')
    return binary


def elf_needed(path):
    text = capture(['readelf', '-dW', path])
    return re.findall(r'\(NEEDED\).*?\[(.*?)\]', text)


def package_runtime(bundle, sysroot, architecture):
    triplet = ARCHITECTURES[architecture]['triplet']
    library_dirs = [sysroot / 'usr/lib' / triplet, sysroot / 'lib' / triplet,
                    Path('/usr/lib') / triplet, Path('/lib') / triplet]
    pending = [ARCHITECTURES[architecture]['loader']]
    for binary in (bundle / 'bin').iterdir():
        pending.extend(elf_needed(binary))
    copied = {}
    while pending:
        soname = pending.pop()
        if soname in copied:
            continue
        if not re.fullmatch(r'[a-zA-Z0-9_.+-]+', soname):
            raise RuntimeError(f'Unsafe library name: {soname}')
        candidates = [base / soname for base in library_dirs]
        source = next((p for p in candidates if p.exists()), None)
        if source is None:
            raise RuntimeError(f'Missing DT_NEEDED library: {soname}')
        dest = bundle / 'lib' / soname
        shutil.copyfile(source.resolve(), dest)
        dest.chmod(0o755)
        copied[soname] = source.resolve()
        pending.extend(elf_needed(dest))
    # Legal attribution is part of the hashed payload, not a separate asset.
    packages = []
    provenance_path = sysroot / 'screen-build-packages.json'
    provenance = json.loads(provenance_path.read_text()) if provenance_path.exists() else {}
    for soname in sorted(copied):
        package = PACKAGES.get(soname)
        if package is None:
            raise RuntimeError(f'Add license/source attribution for {soname}')
        copyright_file = next((p for p in [sysroot / 'usr/share/doc' / package / 'copyright',
                                           Path('/usr/share/doc') / package / 'copyright'] if p.exists()), None)
        if copyright_file is None:
            raise RuntimeError(f'Missing copyright for {package}')
        shutil.copyfile(copyright_file, bundle / 'licenses' / f'{package}.copyright')
        # Debian copyright notices refer to full texts outside the package.
        # Ship those texts too; the relocated payload has no /usr/share contract.
        for license_name in set(re.findall(r'/usr/share/common-licenses/([A-Za-z0-9_.+-]+)', copyright_file.read_text())):
            sources = [sysroot / 'usr/share/common-licenses' / license_name,
                       Path('/usr/share/common-licenses') / license_name]
            license_source = next((p for p in sources if p.is_file()), None)
            if license_source is None:
                raise RuntimeError(f'Missing full license text: {license_name}')
            shutil.copyfile(license_source, bundle / 'licenses' / f'common-{license_name}.txt')
        if package in provenance:
            origin = provenance[package]
        else:
            query = capture(['dpkg-query', '-W', '-f=${Version}\t${source:Package}\t${source:Version}', package]).split('\t')
            origin = {'version':query[0], 'source_package':query[1], 'source_version':query[2],
                      'metadata_origin':'build-host dpkg database'}
        packages.append({'library':soname, 'original_filename':copied[soname].name, 'package':package,
                         'provenance':origin,
                         'source':'https://launchpad.net/ubuntu/+source/' + origin['source_package'] + '/' + origin['source_version'],
                         'license_file':'licenses/' + package + '.copyright'})
    return packages


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--work-dir', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--sysroot', type=Path, default=Path('/'))
    parser.add_argument('--zig', help='Existing checksum-verified Zig 0.16.0 compiler')
    parser.add_argument('--meson', default='meson', help='Meson command (supports multiple arguments)')
    parser.add_argument('--ninja', default='ninja')
    args = parser.parse_args()
    architecture = platform.machine()
    if platform.system() != 'Linux' or architecture not in ARCHITECTURES:
        parser.error('This payload requires native x86_64 or aarch64 Linux')
    target = ARCHITECTURES[architecture]
    root = args.work_dir.resolve(); root.mkdir(parents=True, exist_ok=True)
    output = args.output.resolve(); output.parent.mkdir(parents=True, exist_ok=True)
    sysroot = args.sysroot.resolve()
    env = os.environ.copy()
    env.pop('LD_LIBRARY_PATH', None)
    env['ZIG_GLOBAL_CACHE_DIR'] = str(root / 'zig-cache')
    env['PKG_CONFIG_PATH'] = str(sysroot / 'usr/lib' / target['triplet'] / 'pkgconfig') + ':' + str(sysroot / 'usr/share/pkgconfig')
    env['PKG_CONFIG_SYSROOT_DIR'] = str(sysroot)
    env['PKG_CONFIG_ALLOW_SYSTEM_LIBS'] = '1'
    env['PKG_CONFIG_ALLOW_SYSTEM_CFLAGS'] = '1'
    zig = zig_compiler(root, args.zig, architecture)
    waymote = checkout(root, 'waymote', WAYMOTE_URL, WAYMOTE_REV)
    # Add only an install step for streamd; do not build or ship the Go gateway.
    run(['git', '-C', waymote, 'diff', '--exit-code', '--', '.', ':(exclude)build.zig'])
    original = capture(['git', '-C', waymote, 'show', 'HEAD:build.zig'])
    needle = '    b.installArtifact(streamd);'
    if original.count(needle) != 1:
        raise RuntimeError('Pinned Waymote build shape changed')
    patched = original.replace(needle, needle + '\n    b.step("screen-helper", "Build only streamd").dependOn(&b.addInstallArtifact(streamd, .{}).step);')
    (waymote / 'build.zig').write_text(patched)
    run([zig, 'build', 'screen-helper', '-j2', '-Dcpu=baseline', '-Doptimize=ReleaseFast'], cwd=waymote, env=env)
    grim = checkout(root, 'grim', GRIM_URL, GRIM_REV, GRIM_TAG)
    protocols = checkout(root, 'protocols', PROTOCOLS_URL, PROTOCOLS_REV, 'refs/tags/1.49')
    run(['git', '-C', grim, 'diff', '--exit-code'])
    run(['git', '-C', protocols, 'diff', '--exit-code'])
    # Native scanner/data variables are not automatically prefixed by pkg-config.
    pcdir = root / 'pkgconfig'; pcdir.mkdir(exist_ok=True)
    scanner = sysroot / 'usr/bin/wayland-scanner'
    (pcdir / 'wayland-protocols.pc').write_text(f'Name: wayland-protocols\nDescription: pinned protocol XML\nVersion: 1.49\npkgdatadir={protocols}\n')
    (pcdir / 'wayland-scanner.pc').write_text(f'Name: wayland-scanner\nDescription: native Wayland scanner\nVersion: 1.20.0\nwayland_scanner={scanner}\n')
    env['PKG_CONFIG_PATH'] = str(pcdir) + ':' + env['PKG_CONFIG_PATH']
    env['CFLAGS'] = target['cflags']
    env['NINJA'] = args.ninja
    build = root / 'grim-build'
    if build.exists():
        shutil.rmtree(build)
    run(shlex.split(args.meson) + ['setup', build, grim, '--buildtype=release', '-Djpeg=disabled', '-Dman-pages=disabled'], env=env)
    run([args.ninja, '-C', build], env=env)
    bundle = root / 'bundle'
    if bundle.exists():
        shutil.rmtree(bundle)
    for name in ['bin', 'lib', 'licenses']:
        (bundle / name).mkdir(parents=True, exist_ok=True)
    shutil.copyfile(waymote / 'zig-out/bin/waymote-streamd', bundle / 'bin/waymote-streamd')
    shutil.copyfile(build / 'grim', bundle / 'bin/grim')
    for binary in (bundle / 'bin').iterdir():
        binary.chmod(0o755)
    libraries = package_runtime(bundle, sysroot, architecture)
    for name, source, license_name in [('waymote', waymote, 'LICENSE'), ('grim', grim, 'LICENSE'), ('wayland-protocols', protocols, 'COPYING')]:
        shutil.copyfile(source / license_name, bundle / 'licenses' / f'{name}.txt')
    upstream = {'waymote':{'url':WAYMOTE_URL, 'revision':WAYMOTE_REV, 'build_patch':'streamd-only install step; no runtime source modifications'},
                'grim':{'url':GRIM_URL, 'tag':GRIM_TAG, 'revision':GRIM_REV},
                'wayland_protocols':{'url':PROTOCOLS_URL, 'revision':PROTOCOLS_REV},
                'zig':{'version':ZIG_VERSION, 'archive_sha256':target['zig_sha256']}, 'cpu':architecture + ' baseline',
                'runtime_libraries':libraries,
                'build_tools':{'meson':capture(shlex.split(args.meson) + ['--version']).strip(),
                               'ninja':capture([args.ninja, '--version']).strip(),
                               'cc':capture(['cc', '--version']).splitlines()[0]},
                'ffmpeg':'not included; Hand system FFmpeg bridge'}
    (bundle / 'upstream.json').write_text(json.dumps(upstream, indent=2) + '\n')
    files = []
    for path in sorted(bundle.rglob('*')):
        if path.is_file():
            mode = 0o755 if path.parent.name in ['bin', 'lib'] else 0o644
            path.chmod(mode)
            files.append({'path':path.relative_to(bundle).as_posix(), 'sha256':hashlib.sha256(path.read_bytes()).hexdigest(),
                          'bytes':path.stat().st_size, 'mode':mode})
    (bundle / 'manifest.json').write_text(json.dumps({'version':1, 'architecture':architecture, 'files':files}, indent=2) + '\n')
    (bundle / 'manifest.json').chmod(0o644)
    expanded = sum(p.stat().st_size for p in bundle.rglob('*') if p.is_file())
    if expanded > 128 * 1024 * 1024:
        raise RuntimeError('Expanded bundle exceeds 128 MiB')
    with tempfile.NamedTemporaryFile(dir=output.parent, delete=False) as tmp:
        temporary = Path(tmp.name)
        try:
            with gzip.GzipFile(fileobj=tmp, mode='wb', filename='', mtime=0, compresslevel=9) as gz:
                with tarfile.open(fileobj=gz, mode='w', format=tarfile.USTAR_FORMAT) as tar:
                    ordered = [bundle / 'manifest.json'] + [p for p in sorted(bundle.rglob('*')) if p.name != 'manifest.json']
                    for path in ordered:
                        if not path.is_file():
                            continue
                        info = tar.gettarinfo(str(path), arcname=path.relative_to(bundle).as_posix())
                        info.uid = info.gid = 0; info.uname = info.gname = ''; info.mtime = 0
                        with path.open('rb') as stream:
                            tar.addfile(info, stream)
            if temporary.stat().st_size > 64 * 1024 * 1024:
                raise RuntimeError('Compressed bundle exceeds 64 MiB')
            os.replace(temporary, output)
        finally:
            temporary.unlink(missing_ok=True)
    fixture = Path(__file__).resolve().parent / 'tests/linux-screen-helpers-bundle.py'
    run(['python3', fixture, output])
    print(json.dumps({'bundle':str(output), 'sha256':hashlib.sha256(output.read_bytes()).hexdigest(),
                      'compressed_bytes':output.stat().st_size, 'expanded_bytes':expanded}))


if __name__ == '__main__':
    main()
