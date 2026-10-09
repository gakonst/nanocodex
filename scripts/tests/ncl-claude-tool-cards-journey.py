#!/usr/bin/env python3
"""Unified local TUI (ncl --claude) tool-card journey for Claude-native file, search and plan tools.

Real ncl over a PTY with the real Code Mode exec runtime and the real native
Write/Edit/Read/Grep/Glob/TodoWrite adapters acting on a temporary workspace;
only the Claude Messages HTTP provider is synthetic. Two exec cells run the
tools. The journey asserts the collapsed batch rows (path, range, match
counts, plan progress, failure text, no raw argument JSON or large payload),
then expands every card with Ctrl+O and asserts the Edit hunk, the
non-diff Write review and the bounded large-file body.
"""
from claude_code_fixture import normalize_request
import argparse, importlib.util, json, os, re, threading, time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

spec = importlib.util.spec_from_file_location('features', Path(__file__).with_name('ncl-local-features-journey.py'))
features = importlib.util.module_from_spec(spec); spec.loader.exec_module(features)
Session, require, sse = features.Session, features.require, features.sse

PROMPT = 'exercise claude tool cards'
# A second user turn keeps each batch within the six visible rows.
PLAN_PROMPT = 'review the plan card batch'
FILES_CODE = '''await tools.Write({file_path: "notes/cards.txt", content: "alpha\\nbeta\\ngamma\\n"});
await tools.Edit({file_path: "notes/cards.txt", old_string: "beta", new_string: "BETA_EDITED"});
await tools.Read({file_path: "notes/cards.txt", offset: 2, limit: 2});
await tools.Grep({pattern: "BETA_EDITED", output_mode: "content"});
await tools.Glob({pattern: "**/*.txt"});
text("FILES_CELL_DONE");'''
PLAN_CODE = '''await tools.TodoWrite({todos: [
  {content: "Write cards", status: "completed", activeForm: "Writing cards"},
  {content: "Review cards", status: "in_progress", activeForm: "Reviewing cards"}]});
await tools.Write({file_path: "notes/large.txt", content: Array.from({length: 400}, (_, i) => "LARGE_LINE_" + i).join("\\n") + "\\n"});
let failure = "NO_FAILURE";
try { await tools.Edit({file_path: "notes/cards.txt", old_string: "MISSING_NEEDLE", new_string: "x"}); }
catch (error) { failure = "EDIT_REJECTED"; }
text("PLAN_CELL_DONE " + failure);'''


def text_of(message):
    content = message.get('content')
    return content if isinstance(content, str) else json.dumps(content)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, default=Path('output/ncl-claude-tool-cards') / uuid4().hex)
    args = parser.parse_args(); artifact = args.output.resolve(); artifact.mkdir(parents=True)
    workspace = artifact / 'workspace'; workspace.mkdir()
    home = artifact / 'home'; (home / 'codex').mkdir(parents=True)
    env = {'HOME': str(home), 'CODEX_HOME': str(home / 'codex'), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'TERM': 'xterm-256color', 'NANOCODEX_COMPUTER': 'off'}
    lock = threading.Lock(); requests, errors = [], []

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_POST(self):
            body = normalize_request(json.loads(self.rfile.read(int(self.headers['content-length']))), artifact)
            last = text_of(body['messages'][-1]) if body['messages'] else ''
            try:
                if 'PLAN_CELL_DONE' in last:
                    block, role = {'type': 'text', 'text': 'CARDS_TURN_DONE'}, 'final'
                elif 'FILES_CELL_DONE' in last:
                    block, role = {'type': 'text', 'text': 'FILES_TURN_DONE'}, 'files-final'
                elif 'tool_result' in last:
                    raise AssertionError(f'unexpected tool result: {last[:400]}')
                elif PLAN_PROMPT in last:
                    block, role = {'type': 'tool_use', 'id': 'plan-cell', 'name': 'exec', 'input': {'code': PLAN_CODE}}, 'plan-cell'
                else:
                    require(PROMPT in last, 'unexpected request')
                    block, role = {'type': 'tool_use', 'id': 'files-cell', 'name': 'exec', 'input': {'code': FILES_CODE}}, 'files-cell'
            except Exception as error:
                errors.append(str(error)); block, role = {'type': 'text', 'text': 'fixture-error'}, 'error'
            with lock:
                requests.append({'role': role, 'last': last[:4000]})
                (artifact / 'provider.json').write_text(json.dumps(requests, indent=2))
            response = sse(block, body['model'])
            self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.send_header('Content-Length', str(len(response))); self.end_headers(); self.wfile.write(response)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider); threading.Thread(target=server.serve_forever, daemon=True).start()
    binary = args.binary.resolve()
    # The local TUI is the ncl entry point; the shipped nanocodex binary selects it with a leading --local.
    entry = [] if binary.stem.lower() == 'ncl' else ['--local']
    command = [str(binary), *entry, '--claude', '--model', 'claude-sonnet-5-5', '--claude-api-key', 'synthetic-key',
               '--claude-messages-url', f'http://127.0.0.1:{server.server_port}/v1/messages', '--cwd', str(workspace), '--browser=none',
               '--mcp-defaults', 'false', '--mcp-codex-config', 'false', '--web-search', 'false', '--image-generation', 'false',
               '--subagents', 'false', '--memory', 'false']
    checks = []; outcome = {'success': False}; session = None
    try:
        session = Session('ncl', command, workspace, env, artifact)
        session.wait(lambda: 'claude-sonnet-5-5' in session.screen.text() and '0%/' in session.screen.text(), 'composer absent', 60); time.sleep(1.5)
        session.enter(PROMPT)
        session.wait(lambda: session.shown('FILES_TURN_DONE'), 'files turn did not finish', 60); time.sleep(1)
        session.enter(PLAN_PROMPT)
        session.wait(lambda: session.shown('CARDS_TURN_DONE'), 'plan turn did not finish', 60)
        require(not errors, f'fixture errors: {errors}')
        require((workspace / 'notes/cards.txt').read_text() == 'alpha\nBETA_EDITED\ngamma\n', 'real Write/Edit effect missing')
        require((workspace / 'notes/large.txt').read_text().count('LARGE_LINE_') == 400, 'large Write effect missing')
        require('EDIT_REJECTED' in requests[-1]['last'], 'missing old_string did not fail the real Edit')
        checks.append('real exec cells wrote, edited and read workspace files; the Edit with a missing old_string was rejected')
        time.sleep(1); session.drain()
        collapsed = session.screen.text(); (artifact / 'collapsed.screen.txt').write_text(collapsed)
        flat = re.sub(r'\s+', ' ', collapsed)
        expected_rows = {
            'Write row: path and size, not a diff': r'Write notes/cards\.txt · 3 lines · 17 B',
            'Edit row: path and hunk counts': r'Edit notes/cards\.txt · \+1 −1',
            'Read row: path and returned range': r'Read notes/cards\.txt · lines 2–3',
            'Grep row: pattern and match count': r'Grep "BETA_EDITED" · 1 matching line',
            'Glob row: pattern and file count': r'Glob \*\*/\*\.txt · 1 file',
            'TodoWrite row: plan progress and current step': r'Plan 1/2 complete · Reviewing cards',
            'large Write row: line count only': r'Write notes/large\.txt · 400 lines',
            'failed Edit row: failure text visible': r'× Edit notes/cards\.txt · \+1 −1 · old_string not found',
        }
        for label, pattern in expected_rows.items():
            require(re.search(pattern, flat), f'{label} absent from collapsed screen: {pattern}')
        for raw in ['"old_string"', '"file_path"', '"todos"', 'LARGE_LINE_', 'MISSING_NEEDLE', '{"content"']:
            require(raw not in collapsed, f'collapsed rows leak raw payload {raw!r}')
        checks.append('collapsed batch rows show path, range, counts, plan progress and the Edit failure; no raw argument JSON, old_string or large payload')

        os.write(session.master, b'\x0f'); time.sleep(1); session.drain()
        for _ in range(40):
            os.write(session.master, b'\x1b[5~'); time.sleep(.15); session.drain()
        frames = '\n=====\n'.join(session.seen[-200:]); (artifact / 'expanded.frames.txt').write_text(frames)
        expected_expanded = {
            'Edit hunk header with path and counts': 'notes/cards.txt · +1 −1',
            'Edit hunk relative-line label': 'relative lines',
            'Edit removed line': '- beta',
            'Edit added line': '+ BETA_EDITED',
            'Edit footer': 'replacement applied · surrounding file not shown',
            'Write footer: full contents, not a diff': 'full contents written · previous contents not shown',
            'bounded large body': 'LARGE_LINE_79',
            'large body omission note': '320 more lines not shown',
            'Read output with line numbers': '2\tBETA_EDITED'.replace('\t', ''),
            'Grep options': 'content',
            'TodoWrite checklist': '◐ Review cards',
            'failed Edit error text': 'old_string not found',
        }
        for label, text in expected_expanded.items():
            require(session.shown(text), f'{label} absent from expanded cards: {text!r}')
        require(not session.shown('LARGE_LINE_399'), 'large Write body was not bounded')
        checks.append('Ctrl+O expands every card: Edit shows a real replacement hunk, Write shows bounded new contents with the not-a-diff footer, failures keep the error')
        session.quit()
        outcome = {'success': True, 'checks': checks, 'provider_requests': [r['role'] for r in requests]}
    except Exception as error:
        outcome['error'] = str(error); outcome['fixture_errors'] = errors
        raise
    finally:
        if session: session.close()
        (artifact / 'scenario.json').write_text(json.dumps({'command': command, 'environment': env, 'prompts': [PROMPT, PLAN_PROMPT],
            'cells': [FILES_CODE, PLAN_CODE], 'boundary': 'actual ncl TUI via PTY, real Code Mode and native Claude tools; synthetic Messages HTTP only'}, indent=2))
        (artifact / 'outcome.json').write_text(json.dumps(outcome, indent=2)); server.shutdown(); print(json.dumps({'artifact': str(artifact), **outcome}))

if __name__ == '__main__': main()
