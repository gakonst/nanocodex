#!/usr/bin/env python3
"""Opt-in tmux popup; metadata only, never capture-pane or model calls."""
import argparse
import concurrent.futures
import curses
import json
import os
import subprocess
import time
import unicodedata

FORMAT = '\t'.join(('#{pane_id}', '#{session_id}', '#{session_name}', '#{window_index}',
                    '#{pane_index}', '#{pane_current_command}', '#{pane_title}',
                    '#{@nanocodex-overview}'))


def clean(value, limit=512):
    return ''.join(c if unicodedata.category(c)[0] != 'C' else ' '
                   for c in str(value))[:limit]


def command(*args, timeout=2):
    return subprocess.run(args, capture_output=True, text=True, timeout=timeout,
                          check=True).stdout


def parse_panes(output, now=None):
    now = time.time() * 1000 if now is None else now
    rows = []
    for line in output.splitlines():
        fields = line.split('\t', 7)
        if len(fields) != 8:
            continue
        pane, session, name, window, index, process, title, raw = fields
        if not (pane.startswith('%') and pane[1:].isdigit()
                and session.startswith('$') and session[1:].isdigit()):
            continue
        metadata = {}
        try:
            candidate = json.loads(raw)
            age = now - float(candidate.get('updated_at', 0))
            if candidate.get('version') == 1 and 0 <= age <= 10000:
                metadata = candidate
        except (ValueError, TypeError, AttributeError):
            pass
        rows.append(dict(pane=pane, session=session, location=clean(f'{name}:{window}.{index}'),
                         process=clean(process), title=clean(title), metadata=metadata))
    return rows


def load_summaries(binary):
    try:
        return json.loads(command(binary, 'list', timeout=6)).get('summaries', {})
    except (OSError, subprocess.SubprocessError, ValueError, AttributeError):
        return None


def describe(row, summaries):
    metadata = row['metadata']
    if not metadata:
        return row['process'], row['title'], ''
    agent = metadata.get('agent_id', '')
    summary = summaries.get(agent, {}) if isinstance(summaries, dict) else {}
    summary = summary if isinstance(summary, dict) else {}
    presentation = summary.get('presentation') or {}
    presentation = presentation if isinstance(presentation, dict) else {}
    title = clean(presentation.get('title') or summary.get('title') or agent or 'Connecting agent')
    activity = presentation.get('activity') or ''
    if isinstance(activity, dict):
        activity = activity.get('text') or activity.get('label') or activity.get('summary') or ''
    status = clean(metadata.get('status', 'unknown'))
    if status == 'idle' and presentation.get('status') in ('completed', 'cancelled', 'failed'):
        status = presentation['status']
    if activity and status == 'running':
        status += ' · ' + clean(activity)
    local_prompt = metadata.get('prompt') or ''
    remote_prompt = presentation.get('lastUserPrompt') or ''
    try:
        remote_newer = float(presentation.get('lastUserMessageAt', 0)) > float(metadata.get('prompt_at', 0))
    except (TypeError, ValueError):
        remote_newer = False
    prompt = clean(remote_prompt if remote_prompt and remote_newer else local_prompt or remote_prompt)
    return status, title, prompt


def jump(row, client):
    # Validated tmux targets go through argv, never a shell.
    args = ['tmux', 'switch-client']
    if client:
        args += ['-c', client]
    args += ['-t', row['session'], ';', 'select-window', '-t', row['pane'],
             ';', 'select-pane', '-t', row['pane']]
    command(*args)


def draw(screen, y, text, attr=0):
    height, width = screen.getmaxyx()
    if y < height and width > 1:
        try:
            screen.addnstr(y, 0, clean(text, 2000), width - 1, attr)
        except curses.error:
            pass


def overview(screen, args):
    try:
        curses.curs_set(0)
    except curses.error:
        pass
    screen.timeout(200)
    selected, query, rows, summaries, error = None, '', [], {}, ''
    next_panes, next_summary, pending = 0, 0, None
    pool = concurrent.futures.ThreadPoolExecutor(max_workers=1)
    try:
        while True:
            now = time.monotonic()
            if now >= next_panes:
                try:
                    rows = parse_panes(command('tmux', 'list-panes', '-a', '-F', FORMAT))
                    error = ''
                except (OSError, subprocess.SubprocessError) as exc:
                    rows, error = [], f'tmux unavailable: {exc}'
                next_panes = now + 1
            if pending is not None and pending.done():
                summaries = pending.result() or {}
                pending = None
            if now >= next_summary and pending is None:
                pending = pool.submit(load_summaries, args.nanocodex)
                next_summary = now + 5
            visible = [(row, describe(row, summaries)) for row in rows]
            visible = [(row, detail) for row, detail in visible
                       if query.casefold() in (' '.join(detail) + row['location']).casefold()]
            ids = [row['pane'] for row, _ in visible]
            if selected not in ids:
                selected = ids[0] if ids else None
            index = ids.index(selected) if selected else 0
            screen.erase()
            draw(screen, 0, 'Agent overview · ↑/↓ select · Enter jump · Esc quit', curses.A_BOLD)
            draw(screen, 1, f'Filter: {query}   | {len(visible)}/{len(rows)} panes')
            height, _ = screen.getmaxyx()
            page = max(1, (height - 4) // 3)
            start = max(0, index - page + 1)
            for offset, (row, (status, title, prompt)) in enumerate(visible[start:start + page]):
                attr = curses.A_REVERSE if row['pane'] == selected else 0
                draw(screen, 3 + offset * 3, f"{row['location']} {row['pane']}  [{status}]  {title}", attr)
                draw(screen, 4 + offset * 3, f'  Prompt: {prompt}' if prompt else '  —', attr)
            if error:
                draw(screen, 2, error)
            screen.refresh()
            key = screen.getch()
            if key in (27, 3):
                return
            if key in (10, 13, curses.KEY_ENTER) and selected:
                try:
                    jump(visible[index][0], args.client)
                    return
                except (OSError, subprocess.SubprocessError):
                    error = 'Pane closed or client unavailable; refresh and retry.'
                    next_panes = 0
            elif key in (curses.KEY_UP, curses.KEY_DOWN) and ids:
                selected = ids[(index + (1 if key == curses.KEY_DOWN else -1)) % len(ids)]
            elif key in (curses.KEY_BACKSPACE, 127, 8):
                query = query[:-1]
            elif 32 <= key <= 126:
                query += chr(key)
    finally:
        pool.shutdown(wait=False, cancel_futures=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--client', help='tmux client name supplied by key binding')
    parser.add_argument('--nanocodex', default='nanocodex2')
    args = parser.parse_args()
    if not os.environ.get('TMUX'):
        parser.error('run inside tmux (see docs/tmux-agent-overview.md)')
    curses.wrapper(overview, args)


if __name__ == '__main__':
    main()

