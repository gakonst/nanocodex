import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, stat, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { createClaudeTools } from '../node/claude-tools.mjs';

async function fixture(t) {
  const workspace = await mkdtemp(join(tmpdir(), 'native-claude-tools-'));
  const adapter = await createClaudeTools({ workspace });
  t.after(async () => { await adapter.close(); await rm(workspace, { recursive: true, force: true }); });
  return { workspace, adapter, tools: Object.fromEntries(adapter.tools.map(tool => [tool.name, tool.handler])) };
}

test('native Claude workspace journey: write, numbered read, exact edit, search, and notebook mutation', async t => {
  const { workspace, tools } = await fixture(t);
  assert.equal(await tools.Write({ file_path: 'src/test.txt', content: 'begin\nHit one\nbetween\nHit two\nend\n' }), 'Wrote src/test.txt');
  assert.equal(await tools.Read({ file_path: join(workspace, 'src/test.txt'), offset: 2, limit: 2 }), '2\tHit one\n3\tbetween\n');
  await assert.rejects(tools.Edit({ file_path: 'src/test.txt', old_string: 'Hit', new_string: 'found' }), /occurs 2 times/);
  assert.equal(await tools.Edit({ file_path: 'src/test.txt', old_string: 'Hit', new_string: 'found', replace_all: true }), 'Edited src/test.txt (2 replacement(s))');
  assert.equal(await tools.Glob({ pattern: '**/t?st.txt' }), 'src/test.txt\n');
  assert.equal(await tools.Grep({ pattern: 'found' }), 'src/test.txt\n');
  assert.equal(await tools.Grep({ pattern: 'found', output_mode: 'count' }), 'src/test.txt:2\n');
  assert.equal(await tools.Grep({ pattern: 'found', output_mode: 'content', '-C': 1, head_limit: 1 }), 'src/test.txt-1-begin\nsrc/test.txt:2:found one\nsrc/test.txt-3-between\n');
  assert.equal(await tools.Grep({ pattern: 'found', output_mode: 'content', '-o': true, '-n': false, head_limit: 1 }), 'src/test.txt:found\n');
  assert.equal(await tools.Grep({ pattern: 'begin.*between', multiline: true, output_mode: 'content' }), 'src/test.txt:1:begin\nsrc/test.txt:2:found one\nsrc/test.txt:3:between\n');
  await assert.rejects(tools.Grep({ pattern: '(?=found)' }), /invalid or oversized regex/);
  await assert.rejects(tools.Grep({ pattern: 'found', type: 'txt' }), /unsupported Grep option/);
  await assert.rejects(tools.Read({ file_path: '../outside' }), /traversal/);
  await assert.rejects(tools.Glob({ pattern: '**/*.{txt,rs}' }), /unsupported glob/);
  const notebook = { cells: [{ id: 'first', cell_type: 'code', metadata: { retained: true }, execution_count: 1, outputs: [{ output_type: 'stream', text: 'old' }], source: ['print(1)\n'] }], metadata: { language_info: { name: 'python' } }, nbformat: 4, nbformat_minor: 5 };
  await tools.Write({ file_path: 'book.ipynb', content: JSON.stringify(notebook) });
  const receipt = JSON.parse(await tools.NotebookEdit({ notebook_path: 'book.ipynb', cell_id: 'first', new_source: '# Title\n', cell_type: 'markdown' }));
  assert.equal(receipt.old_source, 'print(1)\n');
  assert.equal(receipt.language, 'python');
  const saved = JSON.parse(await readFile(join(workspace, 'book.ipynb'), 'utf8'));
  assert.deepEqual(saved.cells[0].metadata, { retained: true });
  assert.equal(saved.cells[0].outputs, undefined);
  assert.deepEqual(saved.cells[0].source, ['# Title\n']);
  const inserted = JSON.parse(await tools.NotebookEdit({ notebook_path: 'book.ipynb', new_source: 'pass', edit_mode: 'insert', cell_type: 'code' }));
  assert.equal(inserted.cell_type, 'code');
  await tools.NotebookEdit({ notebook_path: 'book.ipynb', cell_id: inserted.cell_id, new_source: '', edit_mode: 'delete' });
  assert.equal(JSON.parse(await readFile(join(workspace, 'book.ipynb'), 'utf8')).cells.length, 1);
  console.log('Native workspace receipt:', JSON.stringify({ read: 'numbered lines 2–3', edit: 'two exact replacements', grep: 'count=2 with context/only-match/multiline', notebook: receipt }));
});

test('native workspace bounds and symlink policy preserve files after rejected writes', async t => {
  const { workspace, tools } = await fixture(t);
  await writeFile(join(workspace, 'executable'), 'hello\n', { mode: 0o755 });
  await tools.Write({ file_path: 'executable', content: 'replacement\n' });
  assert.equal((await stat(join(workspace, 'executable'))).mode & 0o777, 0o755);
  await assert.rejects(tools.Write({ file_path: 'executable', content: 'a'.repeat(1048577) }), /1 MiB/);
  assert.equal(await readFile(join(workspace, 'executable'), 'utf8'), 'replacement\n');
  await symlink('executable', join(workspace, 'link'));
  assert.equal(await tools.Read({ file_path: 'link' }), '1\treplacement\n');
  await assert.rejects(tools.Write({ file_path: 'link', content: 'changed' }), /symlink/);
  assert.equal(await tools.Glob({ pattern: '*' }), 'executable\n');
  await writeFile(join(workspace, 'binary'), Buffer.from([255]));
  await assert.rejects(tools.Read({ file_path: 'binary' }), /UTF-8/);
});

test('native Bash returns separate bounded streams, rejects unavailable flags, and cleans up timeout/cancellation', async t => {
  const { workspace, adapter, tools } = await fixture(t);
  const receipt = JSON.parse(await tools.Bash({ command: 'printf stdout; printf stderr >&2; exit 7' }));
  assert.deepEqual(receipt, { stdout: 'stdout', stderr: 'stderr', exit_code: 7, interrupted: false, truncated: false });
  const large = JSON.parse(await tools.Bash({ command: 'head -c 10000 /dev/zero' }));
  assert.equal(Buffer.byteLength(large.stdout), 4096);
  assert.equal(large.truncated, true);
  for (const input of [{ command: 'true', run_in_background: true }, { command: 'true', dangerouslyDisableSandbox: false }, { command: 'true', cmd: 'false' }]) await assert.rejects(tools.Bash(input));
  // Check process ownership directly. A delayed file write races a busy Node
  // event loop: the shell can finish its delay before the timeout callback runs.
  const descendant = 'printf "%s\\n" "$$" > parent.pid; sleep 30 & printf "%s\\n" "$!" > child.pid; wait';
  await assert.rejects(tools.Bash({ command: descendant, timeout: 1000 }), /timed out/);
  for (const name of ['parent.pid', 'child.pid']) {
    const pid = Number((await readFile(join(workspace, name), 'utf8')).trim());
    assert.ok(Number.isSafeInteger(pid) && pid > 1, `${name} records a started process`);
    const deadline = performance.now() + 2000;
    while (true) {
      try { process.kill(pid, 0); }
      catch (error) { assert.equal(error.code, 'ESRCH'); break; }
      assert.ok(performance.now() < deadline, `${name} process ${pid} was reaped`);
      await delay(20);
    }
  }
  const controller = new AbortController();
  const pending = tools.Bash({ command: 'sleep 20' }, { signal: controller.signal });
  setTimeout(() => controller.abort(new Error('cancel journey')), 30);
  await assert.rejects(pending, /cancel journey/);
  const foreground = performance.now();
  const background = JSON.parse(await tools.Bash({ command: 'sleep 20 & printf done' }));
  assert.equal(background.stdout, 'done');
  assert.ok(performance.now() - foreground < 2000, 'foreground completion cleans up background descendants');
  const closing = tools.Bash({ command: 'sleep 20' });
  const rejected = assert.rejects(closing, /closed/);
  await delay(20);
  await adapter.close();
  await rejected;
  await assert.rejects(tools.Read({ file_path: 'missing' }), /closed/);
  console.log('Native Bash receipt:', JSON.stringify(receipt), 'timeout/cancel/close and descendants cleaned up');
});
