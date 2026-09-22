import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { build } from 'esbuild';
import { createCodeRuntime } from '../runtime/code-runtime.mjs';
import { createWorkerEvaluator } from '../runtime/worker-evaluator.mjs';
import { NodeWebWorker } from './support/node-web-worker.mjs';

const oracle = JSON.parse(await readFile(new URL('../../../crates/nanocodex-tools/src/code_mode/native-behavior.json', import.meta.url)));
for (const keepNames of [false, true]) {
  test(`browser-target minified Worker preserves upstream helpers and tool failures (keepNames=${keepNames})`, async () => {
    const bundle = await build({
      entryPoints: [fileURLToPath(new URL('../runtime/code-evaluator.worker.mjs', import.meta.url))],
      bundle: true, minify: true, keepNames, platform: 'browser', format: 'esm', write: false,
    });
    const url = `data:text/javascript;base64,${Buffer.from(bundle.outputFiles[0].text).toString('base64')}`;
    const runtime = createCodeRuntime({
      echo: { handler: input => input },
      fail: { handler() { throw 'tool failure'; } },
    }, { evaluate: createWorkerEvaluator({ createWorker: () => new NodeWebWorker(url) }) });
    try {
      for (const example of oracle.helpers) {
        const result = JSON.parse(await runtime.executeCode(`${example.kind}(${example.expression});`));
        if ('error' in example.result) {
          assert.equal(result.success, false, example.expression);
          assert.ok(result.output.includes(example.result.error), result.output);
        } else {
          assert.equal(result.success, true, example.expression);
          assert.deepEqual(result.output.at(-1), example.result.item, example.expression);
        }
      }
      const result = JSON.parse(await runtime.executeCode(`
        text(await Promise.allSettled([tools.echo({value:42}), tools.fail({}), tools.missing({})])
          .then(results => results.map(r => r.status === 'fulfilled' ? r.value
            : typeof r.reason === 'string' ? r.reason : {code:r.reason.code,tool:r.reason.tool})));
      `));
      assert.equal(result.success, true, JSON.stringify(result));
      assert.deepEqual(JSON.parse(result.output.at(-1).text), [
        {value:42}, 'tool failure', {code:'TOOL_NOT_AVAILABLE',tool:'missing'},
      ]);
    } finally { runtime.reset(); }
  });
}
