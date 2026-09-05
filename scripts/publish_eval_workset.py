#!/usr/bin/env python3
"""Publish one SQLite evaluation workset to the Cloudflare coordinator.

The operation is restartable. Immutable case records and evidence archives are
uploaded before the D1 board is replaced, so readers never observe pointers to
objects that are not yet in R2. Running SQLite rows become unclaimed at the
Cloudflare boundary and are claimed again only after the explicit cutover.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tarfile
import threading
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Iterable


ARCHIVE_CONTENT_TYPE = "application/x-tar+zstd"
JSON_CONTENT_TYPE = "application/json"
DIRECT_UPLOAD_LIMIT = 100 * 1024 * 1024
MULTIPART_PART_BYTES = 64 * 1024 * 1024
HTTP_TIMEOUT_SECONDS = 180
EXCLUDED_DIRECTORIES = {"tests", "vm", "workspace"}
EVIDENCE_NAMES = {"reward.txt", "test-stdout.txt", "test-stderr.txt"}
EVIDENCE_SUFFIXES = {".json", ".jsonl"}
TASK_FILES = {"task.toml", "instruction.md", "transcript.json", "README.md", "pre_artifacts.sh"}
TASK_DIRECTORIES = {"environment", "tests", "solution", "steps"}
STATE_LOCK = threading.Lock()
ARCHIVE_COMPRESSION_SLOTS = threading.BoundedSemaphore(4)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--db", type=Path, required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--digest")
    parser.add_argument("--origin", default=os.environ.get("NANOCODEX_EVALS_ORIGIN"))
    parser.add_argument("--case-origin", help="Current loopback coordinator origin")
    parser.add_argument("--state-file", type=Path)
    parser.add_argument("--skip-cases", action="store_true")
    parser.add_argument("--skip-artifacts", action="store_true")
    parser.add_argument("--skip-tasks", action="store_true")
    parser.add_argument("--skip-seed", action="store_true")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--concurrency", type=int, default=8)
    args = parser.parse_args()
    if not args.origin:
        parser.error("--origin or NANOCODEX_EVALS_ORIGIN is required")
    if not args.skip_cases and not args.case_origin:
        parser.error("--case-origin is required unless --skip-cases is set")
    if args.concurrency < 1 or args.concurrency > 32:
        parser.error("--concurrency must be between 1 and 32")
    return args


def main() -> None:
    sys.stdout.reconfigure(line_buffering=True)
    args = parse_args()
    token = os.environ.get("NANOCODEX_EVALS_WRITE_TOKEN", "").strip()
    if not token:
        raise SystemExit("NANOCODEX_EVALS_WRITE_TOKEN is required")
    origin = args.origin.rstrip("/")
    connection = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    workset = select_workset(connection, args.profile, args.digest)
    state_file = args.state_file or args.db.parent / f"cloudflare-{workset['digest']}.json"
    state = read_state(state_file, workset["digest"])
    rows = read_coordinates(connection, workset["id"], args.limit)
    definitions = read_definitions(connection, workset["id"])
    print(
        f"Publishing {workset['profile']} {workset['digest']} "
        f"({len(rows)} coordinates)"
    )
    available_cases: set[str] = set()
    available_artifacts: set[str] = set()
    available_tasks: set[str] = set()

    if not args.skip_tasks:
        for definition in definitions:
            task_id = public_id(workset["digest"], definition["name"])
            task_key = f"tasks/{workset['digest']}/{task_id}.tar.zst"
            if not object_exists(origin, token, task_key):
                publish_task_package(
                    origin,
                    token,
                    task_key,
                    Path(definition["root"]),
                    state,
                    state_file,
                )
            available_tasks.add(task_key)

    terminal_rows = [
        row
        for row in rows
        if row["state"] in {"success", "failed"} and row["result_path"]
    ]

    def publish_coordinate(row: sqlite3.Row) -> tuple[str | None, str | None]:
        case_id = public_id(workset["digest"], str(row["id"]))
        try:
            case_key = None
            artifact_key = None
            if not args.skip_cases:
                case_key = f"cases/{workset['digest']}/{case_id}.json"
                if not object_exists(origin, token, case_key):
                    case = get_bytes(
                        f"{args.case_origin.rstrip('/')}/v1/evals/cases/{case_id}",
                        token=None,
                    )
                    put_bytes(origin, token, case_key, JSON_CONTENT_TYPE, case)
            if not args.skip_artifacts:
                artifact_key = (
                    f"attempts/{workset['digest']}/historical/{case_id}/evidence.tar.zst"
                )
                if not object_exists(origin, token, artifact_key):
                    publish_archive(
                        origin,
                        token,
                        artifact_key,
                        Path(row["result_path"]),
                        state,
                        state_file,
                    )
            return case_key, artifact_key
        except BaseException as error:
            raise RuntimeError(
                f"coordinate {row['id']} ({case_id}) publication failed"
            ) from error

    with concurrent.futures.ThreadPoolExecutor(max_workers=args.concurrency) as executor:
        for index, (case_key, artifact_key) in enumerate(
            executor.map(publish_coordinate, terminal_rows),
            1,
        ):
            if case_key:
                available_cases.add(case_key)
            if artifact_key:
                available_artifacts.add(artifact_key)
            if index % 25 == 0:
                print(f"  terminal objects {index}/{len(terminal_rows)}")

    if not args.skip_seed:
        board = build_board(
            connection,
            workset,
            definitions,
            rows,
            available_tasks,
            available_cases,
            available_artifacts,
        )
        response = request_json(
            f"{origin}/v1/worksets",
            token,
            method="PUT",
            body=board,
        )
        print(f"Published D1 board: {json.dumps(response, sort_keys=True)}")


def select_workset(
    connection: sqlite3.Connection,
    profile: str,
    digest: str | None,
) -> sqlite3.Row:
    if digest:
        row = connection.execute(
            "SELECT id, profile, digest, created_at_ms FROM worksets "
            "WHERE profile = ? AND digest = ?",
            (profile, digest),
        ).fetchone()
    else:
        row = connection.execute(
            "SELECT id, profile, digest, created_at_ms FROM worksets "
            "WHERE profile = ? ORDER BY created_at_ms DESC, id DESC LIMIT 1",
            (profile,),
        ).fetchone()
    if row is None:
        raise SystemExit(f"workset not found for profile {profile!r}")
    return row


def read_coordinates(
    connection: sqlite3.Connection,
    workset_id: int,
    limit: int | None,
) -> list[sqlite3.Row]:
    sql = """
        SELECT e.id, e.definition_id, e.family_key, e.repetition, e.state,
               e.claim_id, e.worker, e.started_at_ms, e.finished_at_ms,
               e.result_path, e.error, e.harness, e.model, e.thinking,
               e.web_search, r.status, r.outcome, r.input_tokens,
               r.cached_input_tokens, r.output_tokens,
               r.reasoning_output_tokens, r.total_tokens, r.cost_usd,
               r.agent_duration_ms
        FROM eval_tasks e
        LEFT JOIN coordinate_results r ON r.coordinate_id = e.id
        WHERE e.workset_id = ?
        ORDER BY e.id
    """
    parameters: tuple[Any, ...] = (workset_id,)
    if limit is not None:
        sql += " LIMIT ?"
        parameters = (workset_id, limit)
    return list(connection.execute(sql, parameters))


def read_definitions(connection: sqlite3.Connection, workset_id: int) -> list[sqlite3.Row]:
    return list(
        connection.execute(
            "SELECT id, selector, name, root, digest FROM task_definitions "
            "WHERE workset_id = ? ORDER BY id",
            (workset_id,),
        )
    )


def build_board(
    connection: sqlite3.Connection,
    workset: sqlite3.Row,
    definitions: list[sqlite3.Row],
    rows: list[sqlite3.Row],
    available_tasks: set[str],
    available_cases: set[str],
    available_artifacts: set[str],
) -> dict[str, Any]:
    by_definition: dict[int, list[sqlite3.Row]] = {}
    for row in rows:
        by_definition.setdefault(row["definition_id"], []).append(row)
    tasks = []
    for definition in definitions:
        coordinates = []
        for row in by_definition.get(definition["id"], []):
            case_id = public_id(workset["digest"], str(row["id"]))
            terminal = row["state"] in {"success", "failed"}
            case_key = f"cases/{workset['digest']}/{case_id}.json"
            artifact_key = (
                f"attempts/{workset['digest']}/historical/{case_id}/evidence.tar.zst"
            )
            coordinate: dict[str, Any] = {
                "publicId": case_id,
                "familyKey": row["family_key"],
                "harness": row["harness"],
                "model": row["model"],
                "thinking": row["thinking"],
                "webSearch": bool(row["web_search"]),
                "repetition": row["repetition"],
            }
            if terminal:
                coordinate.update(
                    {
                        "state": row["state"],
                        "claimId": row["claim_id"],
                        "worker": row["worker"] or "historical-import",
                        "startedAtMs": row["started_at_ms"],
                        "finishedAtMs": row["finished_at_ms"],
                        "error": row["error"],
                        "result": {
                            "caseKey": case_key if case_key in available_cases else None,
                            "status": row["status"],
                            "outcome": row["outcome"],
                            "inputTokens": row["input_tokens"],
                            "cachedInputTokens": row["cached_input_tokens"],
                            "outputTokens": row["output_tokens"],
                            "reasoningOutputTokens": row["reasoning_output_tokens"],
                            "totalTokens": row["total_tokens"],
                            "costUsd": row["cost_usd"],
                            "durationMs": row["agent_duration_ms"],
                        },
                    }
                )
                if artifact_key in available_artifacts:
                    coordinate["artifactKey"] = artifact_key
                coordinate = without_none(coordinate)
                coordinate["result"] = without_none(coordinate["result"])
            coordinates.append(coordinate)
        task_id = public_id(workset["digest"], definition["name"])
        tasks.append(
            {
                "publicId": task_id,
                "selector": definition["selector"],
                "name": definition["name"],
                "digest": definition["digest"],
                "taskKey": require_task_key(
                    f"tasks/{workset['digest']}/{task_id}.tar.zst",
                    available_tasks,
                ),
                "coordinates": coordinates,
            }
        )
    return {
        "profile": workset["profile"],
        "digest": workset["digest"],
        "createdAtMs": workset["created_at_ms"],
        "tasks": tasks,
    }


def require_task_key(key: str, available_tasks: set[str]) -> str:
    if key not in available_tasks:
        raise RuntimeError(f"task package was not published before D1 seed: {key}")
    return key


def publish_task_package(
    origin: str,
    token: str,
    key: str,
    source: Path,
    state: dict[str, Any],
    state_file: Path,
) -> None:
    if not source.is_dir():
        raise RuntimeError(f"task package directory is unavailable: {source}")
    state_file.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix="nanocodex-task-", suffix=".tar.zst", dir=state_file.parent
    )
    os.close(descriptor)
    temporary = Path(temporary_name)
    try:
        write_task_archive(source, temporary)
        upload_archive_file(origin, token, key, temporary, state, state_file)
    finally:
        temporary.unlink(missing_ok=True)


def publish_archive(
    origin: str,
    token: str,
    key: str,
    source: Path,
    state: dict[str, Any],
    state_file: Path,
) -> None:
    if not source.is_dir():
        raise RuntimeError(f"evidence directory is unavailable: {source}")
    state_file.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix="nanocodex-evidence-", suffix=".tar.zst", dir=state_file.parent
    )
    os.close(descriptor)
    temporary = Path(temporary_name)
    try:
        with ARCHIVE_COMPRESSION_SLOTS:
            write_archive(source, temporary)
        upload_archive_file(origin, token, key, temporary, state, state_file)
    finally:
        temporary.unlink(missing_ok=True)


def upload_archive_file(
    origin: str,
    token: str,
    key: str,
    source: Path,
    state: dict[str, Any],
    state_file: Path,
) -> None:
    size = source.stat().st_size
    if size <= DIRECT_UPLOAD_LIMIT:
        with source.open("rb") as contents:
            put_stream(origin, token, key, ARCHIVE_CONTENT_TYPE, contents, size)
    else:
        with STATE_LOCK:
            multipart_upload(origin, token, key, source, state, state_file)


def write_task_archive(source: Path, target: Path) -> None:
    roots = []
    for name in sorted(TASK_FILES | TASK_DIRECTORIES):
        path = source / name
        if not path.exists():
            continue
        nested_paths = [path]
        if path.is_dir():
            nested_paths.extend(path.rglob("*"))
        for nested in nested_paths:
            if nested.is_symlink():
                raise RuntimeError(f"task packages cannot contain symlinks: {nested}")
        roots.append(name)
    with target.open("wb") as output:
        compressor = subprocess.Popen(
            ["zstd", "-3", "--quiet", "--stdout"],
            stdin=subprocess.PIPE,
            stdout=output,
        )
        assert compressor.stdin is not None
        try:
            with tarfile.open(fileobj=compressor.stdin, mode="w|") as archive:
                for name in roots:
                    archive.add(source / name, arcname=name, recursive=True)
            compressor.stdin.close()
        except BaseException:
            compressor.kill()
            raise
        status = compressor.wait()
        if status:
            raise RuntimeError(f"task package compression failed: zstd={status}")


def write_archive(source: Path, target: Path) -> None:
    paths = list(evidence_paths(source))
    with target.open("wb") as output:
        compressor = subprocess.Popen(
            ["zstd", "-3", "--quiet", "--stdout"],
            stdin=subprocess.PIPE,
            stdout=output,
        )
        assert compressor.stdin is not None
        try:
            with tarfile.open(fileobj=compressor.stdin, mode="w|") as archive:
                for path in paths:
                    archive.add(source / path, arcname=path, recursive=False)
            compressor.stdin.close()
        except BaseException:
            compressor.kill()
            raise
        compressor_status = compressor.wait()
        if compressor_status:
            raise RuntimeError(f"evidence archive compression failed: zstd={compressor_status}")


def evidence_paths(source: Path) -> Iterable[str]:
    selected: list[str] = []
    for root, directories, files in os.walk(source):
        directories[:] = sorted(
            directory for directory in directories if directory not in EXCLUDED_DIRECTORIES
        )
        root_path = Path(root)
        for name in sorted(files):
            path = root_path / name
            if path.is_symlink():
                continue
            if path.suffix in EVIDENCE_SUFFIXES or name in EVIDENCE_NAMES:
                selected.append(path.relative_to(source).as_posix())
    return selected


def multipart_upload(
    origin: str,
    token: str,
    key: str,
    source: Path,
    state: dict[str, Any],
    state_file: Path,
) -> None:
    uploads = state.setdefault("uploads", {})
    upload = uploads.get(key)
    if upload is None:
        upload = request_json(
            import_url(origin, "/v1/import/multipart", key),
            token,
            method="POST",
            headers={"content-type": ARCHIVE_CONTENT_TYPE},
        )
        upload["parts"] = []
        uploads[key] = upload
        write_state(state_file, state)
    parts = {int(part["partNumber"]): part for part in upload["parts"]}
    with source.open("rb") as contents:
        part_number = 1
        while chunk := contents.read(MULTIPART_PART_BYTES):
            if part_number not in parts:
                part = request_json(
                    import_url(
                        origin,
                        f"/v1/import/multipart/{urllib.parse.quote(upload['uploadId'], safe='')}/parts/{part_number}",
                        key,
                    ),
                    token,
                    method="PUT",
                    body=chunk,
                )
                parts[part_number] = part
                upload["parts"] = [parts[number] for number in sorted(parts)]
                write_state(state_file, state)
            part_number += 1
    request_json(
        import_url(
            origin,
            f"/v1/import/multipart/{urllib.parse.quote(upload['uploadId'], safe='')}",
            key,
        ),
        token,
        method="POST",
        body={"parts": [parts[number] for number in sorted(parts)]},
    )
    del uploads[key]
    write_state(state_file, state)


def object_exists(origin: str, token: str, key: str) -> bool:
    request = urllib.request.Request(
        import_url(origin, "/v1/import/objects", key),
        method="HEAD",
        headers=auth_headers(token),
    )
    last_error: BaseException | None = None
    for attempt in range(5):
        try:
            with urllib.request.urlopen(request, timeout=120) as response:
                return response.status == 204
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return False
            last_error = response_error("head object", error)
            if not retryable_http_status(error.code):
                raise last_error from error
        except (urllib.error.URLError, TimeoutError) as error:
            last_error = error
        if attempt < 4:
            time.sleep(0.25 * (2**attempt))
    assert last_error is not None
    raise last_error


def put_bytes(origin: str, token: str, key: str, content_type: str, body: bytes) -> None:
    request_json(
        import_url(origin, "/v1/import/objects", key),
        token,
        method="PUT",
        headers={"content-type": content_type},
        body=body,
    )


def put_stream(
    origin: str,
    token: str,
    key: str,
    content_type: str,
    contents: Any,
    size: int,
) -> None:
    request = urllib.request.Request(
        import_url(origin, "/v1/import/objects", key),
        data=contents,
        method="PUT",
        headers={
            **auth_headers(token),
            "content-type": content_type,
            "content-length": str(size),
        },
    )
    perform(request, "put object")


def request_json(
    url: str,
    token: str,
    *,
    method: str,
    headers: dict[str, str] | None = None,
    body: Any = None,
) -> Any:
    data = None
    request_headers = auth_headers(token)
    request_headers.update(headers or {})
    if body is not None:
        if isinstance(body, bytes):
            data = body
        else:
            data = json.dumps(body, separators=(",", ":")).encode()
            request_headers.setdefault("content-type", JSON_CONTENT_TYPE)
    request = urllib.request.Request(url, data=data, method=method, headers=request_headers)
    response = perform(request, method.lower())
    return json.loads(response) if response else None


def get_bytes(url: str, token: str | None) -> bytes:
    headers = auth_headers(token) if token else {}
    return perform(urllib.request.Request(url, headers=headers), "get case")


def perform(request: urllib.request.Request, operation: str) -> bytes:
    last_error: BaseException | None = None
    for attempt in range(5):
        body = request.data
        if hasattr(body, "seek"):
            try:
                body.seek(0)
            except OSError as error:
                raise RuntimeError(f"failed to rewind {operation} body") from error
        try:
            with urllib.request.urlopen(request, timeout=HTTP_TIMEOUT_SECONDS) as response:
                return response.read()
        except urllib.error.HTTPError as error:
            last_error = response_error(operation, error)
            if not retryable_http_status(error.code):
                raise last_error from error
        except (urllib.error.URLError, TimeoutError) as error:
            last_error = error
        if attempt < 4:
            time.sleep(0.25 * (2**attempt))
    assert last_error is not None
    raise last_error


def retryable_http_status(status: int) -> bool:
    return status in {408, 425, 429} or status >= 500


def response_error(operation: str, error: urllib.error.HTTPError) -> RuntimeError:
    detail = error.read(1_000).decode(errors="replace")
    return RuntimeError(f"{operation} failed with HTTP {error.code}: {detail}")


def auth_headers(token: str) -> dict[str, str]:
    return {
        "authorization": f"Bearer {token}",
        "user-agent": "nanocodex-eval-publisher/1",
    }


def import_url(origin: str, path: str, key: str) -> str:
    return f"{origin}{path}?{urllib.parse.urlencode({'key': key})}"


def public_id(*parts: str) -> str:
    digest = hashlib.sha256()
    for part in parts:
        digest.update(part.encode())
        digest.update(b"\0")
    return digest.hexdigest()[:24]


def without_none(value: dict[str, Any]) -> dict[str, Any]:
    return {key: item for key, item in value.items() if item is not None}


def read_state(path: Path, digest: str) -> dict[str, Any]:
    if not path.exists():
        return {"version": 1, "workset": digest, "uploads": {}}
    state = json.loads(path.read_text())
    if state.get("version") != 1 or state.get("workset") != digest:
        raise RuntimeError(f"import state does not describe workset {digest}: {path}")
    return state


def write_state(path: Path, state: dict[str, Any]) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(state, sort_keys=True) + "\n")
    os.replace(temporary, path)


if __name__ == "__main__":
    main()
