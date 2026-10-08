import { DurableObject } from 'cloudflare:workers';
import { createManagedCodeEffectJournal } from '../../src/managed-recovery-safety';
import { managedCodeEvaluator } from '../../src/code-evaluator';
import { createCodeRuntime } from '../../../nanocodex-tools/runtime/code-runtime.mjs';

import { create as createAgent } from '../../../nanocodex/browser/InlineAgent.mjs';
import { hostManaged } from '../../../nanocodex/browser/Transport.mjs';
import rustModule from '../../../nanocodex/pkg-web/nanocodex_bg.wasm';
export class ObservationFixture extends DurableObject {
  runtime; releaseSecond; releaseThird; enteredSecond; enteredThird; loseAck = false;
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec('CREATE TABLE IF NOT EXISTS fixture_dispatch (name TEXT PRIMARY KEY, n INTEGER NOT NULL)');
    const count = name => ctx.storage.sql.exec('INSERT INTO fixture_dispatch VALUES (?,1) ON CONFLICT(name) DO UPDATE SET n=n+1', name);
    this.checkpoint = new Promise(resolve => { this.checkpointed = resolve; });
    const real = createManagedCodeEffectJournal(ctx.storage);
    const journal = { ...real, observations: { ...real.observations, record: async (...args) => {
      await real.observations.record(...args);
      if (this.loseAck) throw Error('fixture terminal ACK lost after real storage.sync');
      if (!JSON.parse(args[2]).cell.running) this.checkpointed();
    } } };
    this.journal = journal;
    const evaluator = managedCodeEvaluator();
    this.runtime = createCodeRuntime({
      first: { handler: async () => { count('first'); return { receipt: 'first' }; } },
      second: { handler: async () => { count('second'); this.enteredSecond(); await new Promise(r => { this.releaseSecond = r; }); return { receipt: 'late-second' }; } },
      third: { handler: async () => { count('third'); this.enteredThird(); await new Promise(r => { this.releaseThird = r; }); return { receipt: 'uncertain-third' }; } },
    }, { effectJournal: journal, effectIdentity: () => ({ operationId: 'original-op', modelCallIndex: 7 }),
      evaluate: (...args) => { count('evaluate'); return evaluator(...args); } });
  }
  async fetch(request) {
    const input = await request.json();
    const action = input.action;
    if (action === 'start') {
      const entered = new Promise(r => { this.enteredSecond = r; });
      this.thirdStarted = new Promise(r => { this.enteredThird = r; });
      const source = input.terminal ? 'text(await tools.second({})); text("final-output");' + (input.failed ? 'throw new Error("background-failure");' : '')
        : 'text(await tools.first({})); text(await tools.second({})); await tools.third({operation_id:"stable-third"});';
      const active = this.runtime.executeCodeObserved(source, '018f1f9a-7b3c-7a07-8000-000000000021', 'origin');
      await entered;
      this.runtime.preempt('018f1f9a-7b3c-7a07-8000-000000000021', 'origin');
      const result = JSON.parse(await active);
      await this.ctx.storage.put('yield', result);
      await this.ctx.storage.sync();
      return Response.json(result);
    }
    if (action === 'advance') {
      this.releaseSecond();
      await this.thirdStarted;
      await this.ctx.storage.sync();
      return Response.json({advanced: true});
    }
    if (action === 'background') {
      this.releaseSecond();
      await this.checkpoint;
      return Response.json({checkpointed:true});
    }
    if (action === 'finish') {
      this.loseAck = true;
      this.releaseSecond();
      try { await this.runtime.waitCodeObserved(JSON.stringify({cell_id:input.id}), '018f1f9a-7b3c-7a07-8000-000000000021', 'final-wait'); }
      catch (error) { return Response.json({code:error.code, message:error.message}); }
      throw Error('terminal ACK injection did not fire');
    }
    if (action === 'wait') {
      return Response.json(JSON.parse(await this.runtime.waitCodeObserved(JSON.stringify({cell_id:input.id, terminate:input.terminate}), input.session ?? '018f1f9a-7b3c-7a07-8000-000000000021', input.call ?? 'wait-after-restart')));
    }
    if (action === 'rust') {
      const requests = [], events = [];
      const agent = await createAgent({
        module: rustModule, sessionId:'018f1f9a-7b3c-7a07-8000-000000000021',
        toolMode:'code', codeEffectJournal:this.journal, codeEvaluator:() => { throw Error('wait evaluated source'); },
        transport:hostManaged({websocketPreconnect:false, websocketWarmup:false,
          createWebSocket() {
            const pair=new WebSocketPair(), client=pair[0],server=pair[1];
            client.accept();server.accept();
            server.addEventListener('message',event=>{
              requests.push(JSON.parse(event.data));
              const output=requests.length===1
                ? [{type:'function_call',name:'wait',call_id:'wasm-wait',arguments:JSON.stringify({cell_id:input.id})}]
                : [{type:'message',role:'assistant',content:[{type:'output_text',text:'RECOVERY_CONSUMED'}]}];
              server.send(JSON.stringify({type:'response.completed',response:{id:'fixture-'+requests.length,status:'completed',output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
            });
            return client;
          }
        }), tools:{}
      });
      agent.events.watch().onEvent(event=>events.push(event));
      try {
        const result=await agent.turn.prompt({input:'Read the retained cell evidence.'}).result();
        return Response.json({finalMessage:result.finalMessage,requests,events});
      } finally { await agent.session.shutdown(); }
    }
    if (action === 'inspect') {
      return Response.json({
        yield: await this.ctx.storage.get('yield'),
        counts: this.ctx.storage.sql.exec('SELECT name,n FROM fixture_dispatch ORDER BY name').toArray(),
        effects: this.ctx.storage.sql.exec('SELECT call_id,state FROM managed_code_effects ORDER BY call_id').toArray(),
        changes: this.ctx.storage.sql.exec('SELECT total_changes() AS n').one().n,
      });
    }
    throw Error('unknown action');
  }
}
export default { fetch(request, env) { return env.SESSION.getByName('fixture').fetch(request); } };
