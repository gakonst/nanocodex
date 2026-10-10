#!/usr/bin/env python3
"""Shared Codex/Claude homes through the shipped CLI, on throwaway homes only.

Build separately, then run:
  python3 scripts/tests/homes-cli-journey.py --binary target/debug/ncl
(the local CLI tree; an `ncl` hard link to the nanocodex binary selects it)

1. `nanocodex homes` previews, `--apply` links, a rerun is idempotent, and a
   dangling natural path is reported (nonzero) and left untouched.
2. With linking disabled, Codex and Claude sessions both receive the user
   instructions of ~/.codex and ~/.claude, and Claude sees user skills of both
   homes. Only the HTTP model providers are synthetic.
3. Both sessions have a Codex-format JSONL mirror and durable history, seen
   through `nanocodex rewind` previews (no private store inspection).
   Evidence: ignored output/homes-cli/<run>/.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

CODEX_MARKER = "codex-home-global-marker-7f3a"
CLAUDE_MARKER = "claude-home-global-marker-91bc"


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def tree(root):
    """Every path under root with its link target, to prove a dry run is inert."""
    entries = {}
    for path in sorted(root.rglob("*")):
        entries[str(path.relative_to(root))] = os.readlink(path) if path.is_symlink() else path.is_dir()
    return entries


def skill(root, name):
    (root / name).mkdir(parents=True)
    (root / name / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: Synthetic {name} for the homes journey.\n---\nUse {name}.\n")


class Providers:
    """Synthetic Responses and Messages endpoints that record every request."""

    def __init__(self):
        self.requests = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):  # noqa: N802
                body = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))))
                owner.requests.append({"path": self.path, "body": body})
                if self.path.endswith("/messages"):
                    payload = owner.messages(body["model"])
                else:
                    payload = owner.responses()
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"

    @staticmethod
    def responses():
        events = [
            {"type": "response.created", "response": {"id": "resp-homes"}},
            {"type": "response.output_item.done", "item": {
                "type": "message", "role": "assistant", "id": "msg-homes",
                "content": [{"type": "output_text", "text": "codex-homes-complete"}]}},
            {"type": "response.completed", "response": {"id": "resp-homes", "usage": {
                "input_tokens": 1, "input_tokens_details": None, "output_tokens": 1,
                "output_tokens_details": None, "total_tokens": 2}}},
        ]
        return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()

    @staticmethod
    def messages(model):
        events = [
            {"type": "message_start", "message": {"id": "msg_" + uuid4().hex, "type": "message", "role": "assistant",
                                                  "model": model, "content": [], "usage": {"input_tokens": 1, "output_tokens": 0}}},
            {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "claude-homes-complete"}},
            {"type": "content_block_stop", "index": 0},
            {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}},
            {"type": "message_stop"},
        ]
        return "".join("data: " + json.dumps(e) + "\n\n" for e in events).encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=Path("output/homes-cli") / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    artifact.mkdir(parents=True)
    commands, checks = [], []
    outcome = {"success": False, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
               "commands": commands, "checks": checks}
    providers = Providers()

    def run(name, command, env, cwd, expect_ok=True):
        commands.append({"name": name, "command": shlex.join(command), "cwd": str(cwd),
                         "env": {k: v for k, v in env.items() if k in ("HOME", "CODEX_HOME", "CLAUDE_CONFIG_DIR", "NANOCODEX_LINK_HOMES")}})
        result = subprocess.run(command, cwd=cwd, env=env, capture_output=True, text=True, timeout=60)
        (artifact / f"{name}.stdout").write_text(result.stdout)
        (artifact / f"{name}.stderr").write_text(result.stderr)
        require((result.returncode == 0) == expect_ok,
                f"{name} exit {result.returncode}: {result.stderr[-2000:]}")
        return result

    def environment(home):
        # Natural defaults: no CODEX_HOME/CLAUDE_CONFIG_DIR, so ~/.codex and ~/.claude resolve from HOME.
        return {"HOME": str(home), "PATH": os.environ.get("PATH", "/usr/bin:/bin"), "TERM": "xterm-256color",
                "NANOCODEX_COMPUTER": "off"}

    try:
        # 1. Natural-path links on a scratch HOME.
        home = artifact / "link-home"
        codex, claude = home / ".codex", home / ".claude"
        codex.mkdir(parents=True)
        (codex / "AGENTS.md").write_text(f"# Global\n{CODEX_MARKER}\n")
        skill(codex / "skills", "codex-skill")
        skill(claude / "skills", "claude-skill")
        env = environment(home)
        before = tree(home)
        preview = json.loads(run("homes-dry-run", [str(binary), "homes", "--json"], env, home).stdout)
        require(preview["codex_home"] == str(codex) and preview["claude_home"] == str(claude),
                f"homes resolved wrongly: {preview}")
        planned = {(a["link"], a["outcome"]) for a in preview["report"]["actions"]}
        expected = {(str(claude / "CLAUDE.md"), "would_create"),
                    (str(claude / "skills/codex-skill"), "would_create"),
                    (str(codex / "skills/claude-skill"), "would_create")}
        require(expected <= planned, f"dry run plan missing links: {planned}")
        require(tree(home) == before, "dry run changed the filesystem")
        checks.append("dry run planned three links and changed nothing")

        applied = json.loads(run("homes-apply", [str(binary), "homes", "--apply", "--json"], env, home).stdout)
        created = {a["link"] for a in applied["report"]["actions"] if a["outcome"] == "created"}
        require({link for link, _ in expected} <= created, f"apply did not create links: {applied}")
        require((claude / "CLAUDE.md").read_text() == (codex / "AGENTS.md").read_text(), "CLAUDE.md does not show AGENTS.md")
        require((codex / "skills/claude-skill/SKILL.md").is_file(), "Claude skill not visible under ~/.codex")
        require((claude / "skills/codex-skill/SKILL.md").is_file(), "Codex skill not visible under ~/.claude")
        checks.append("apply linked instructions and skills both ways")

        linked = tree(home)
        again = json.loads(run("homes-apply-again", [str(binary), "homes", "--apply", "--json"], env, home).stdout)
        outcomes = {a["outcome"] for a in again["report"]["actions"]}
        require(outcomes == {"already_linked"}, f"rerun was not idempotent: {outcomes}")
        require(tree(home) == linked, "rerun changed the filesystem")
        checks.append("second apply reported already_linked and changed nothing")

        human = run("homes-human", [str(binary), "homes"], env, home).stdout
        require(str(codex) in human and str(claude) in human and "already linked" in human, f"human output: {human}")

        dangling = artifact / "dangling-home"
        (dangling / ".codex").mkdir(parents=True)
        (dangling / ".codex/AGENTS.md").write_text("dangling case\n")
        (dangling / ".claude").mkdir()
        os.symlink(dangling / "missing-target.md", dangling / ".claude/CLAUDE.md")
        snapshot = tree(dangling)
        problem = run("homes-dangling", [str(binary), "homes", "--apply"], environment(dangling), dangling, expect_ok=False)
        require("dangling link" in problem.stdout and "need attention" in problem.stderr, "dangling path not reported")
        require(tree(dangling) == snapshot, "dangling path was modified")
        checks.append("dangling natural path reported with nonzero exit and left untouched")

        # 2. Builders receive both homes without links.
        home = artifact / "session-home"
        codex, claude = home / ".codex", home / ".claude"
        workspace = artifact / "workspace"
        for path in (codex, claude, workspace):
            path.mkdir(parents=True)
        (codex / "AGENTS.md").write_text(f"{CODEX_MARKER}\n")
        (claude / "CLAUDE.md").write_text(f"{CLAUDE_MARKER}\n")
        skill(codex / "skills", "codex-user-skill")
        skill(claude / "skills", "claude-user-skill")
        env = {**environment(home), "NANOCODEX_LINK_HOMES": "false"}
        common = ["--browser=none", "--mcp-defaults", "false", "--web-search", "false",
                  "--image-generation", "false", "--subagents", "false", "--memory", "false",
                  "--cwd", str(workspace)]
        first = len(providers.requests)
        result = run("codex-run", [str(binary), "run", "--api-key", "synthetic-test-key", "--api-base-url",
                                   providers.base + "/v1", "--responses-transport", "https", *common,
                                   "CODEX_HOMES_PROMPT"], env, workspace)
        require("codex-homes-complete" in result.stdout, "Codex run did not finish")
        codex_requests = [json.dumps(r["body"]) for r in providers.requests[first:]]
        require(codex_requests and all(CODEX_MARKER in r and CLAUDE_MARKER in r for r in codex_requests),
                "Codex session did not receive instructions from both homes")
        checks.append("Codex request carried ~/.codex/AGENTS.md and ~/.claude/CLAUDE.md")

        first = len(providers.requests)
        result = run("claude-run", [str(binary), "run", "--claude", "--claude-api-key", "synthetic-test-key",
                                    "--claude-messages-url", providers.base + "/v1/messages",
                                    "--mcp-codex-config", "false", "--model", "claude-sonnet-5-5", *common,
                                    "CLAUDE_HOMES_PROMPT"], env, workspace)
        require("claude-homes-complete" in result.stdout, "Claude run did not finish")
        claude_requests = [json.dumps(r["body"].get("system", "")) for r in providers.requests[first:]]
        require(claude_requests, "Claude run sent no provider request")
        for marker in (CODEX_MARKER, CLAUDE_MARKER, "codex-user-skill", "claude-user-skill"):
            require(all(marker in r for r in claude_requests), f"Claude system prompt lacks {marker}")
        checks.append("Claude system prompt carried both homes' instructions and user skills")
        require(not (claude / "skills/codex-user-skill").exists() and not (codex / "skills/claude-user-skill").exists(),
                "--link-homes false still created links")
        (artifact / "provider.json").write_text(json.dumps(providers.requests, indent=2))

        # 3. Public boundaries only: each session has a Codex-format JSONL
        # mirror, and `nanocodex rewind` previews it from the durable store
        # (a rollout-only thread previews as a rollout copy instead).
        rollouts = sorted((codex / "sessions").rglob("rollout-*.jsonl"))
        records = []
        for path in rollouts:
            meta = json.loads(path.read_text().splitlines()[0])
            require(meta["type"] == "session_meta", f"{path} lacks session_meta")
            session = meta["payload"]["id"]
            preview = json.loads(run(f"rewind-preview-{session}",
                                     [str(binary), "rewind", session, "--mode", "conversation"], env, workspace).stdout)
            require(preview.get("session") == session and "checkpoints" in preview,
                    f"session {session} is not in the durable store: {preview}")
            require(len(preview["checkpoints"]) == 1, f"session {session} turns: {preview['checkpoints']}")
            records.append({"session_id": session, "rollout": str(path),
                            "input": preview["checkpoints"][0]["input"]})
        inputs = sorted(json.dumps(record["input"]) for record in records)
        require(len(records) == 2 and any("CODEX_HOMES_PROMPT" in i for i in inputs)
                and any("CLAUDE_HOMES_PROMPT" in i for i in inputs), f"durable sessions: {records}")
        checks.append("both sessions have Codex JSONL mirrors and durable history via `nanocodex rewind`")
        outcome.update(success=True, sessions=records)
    except Exception as error:
        outcome.update(error=str(error))
        raise
    finally:
        providers.server.shutdown()
        (artifact / "outcome.json").write_text(json.dumps(outcome, indent=2))
        print(json.dumps({"artifact": str(artifact), **outcome}))


if __name__ == "__main__":
    main()
