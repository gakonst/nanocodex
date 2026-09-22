import { env, runInDurableObject } from 'cloudflare:test';
import { expect, it } from 'vitest';
import type { DurableAgentSession } from '../src/index';
it('only the configured admin and original parent can resume email work', async () => {
 const sessions=(env as unknown as {NANOCODEX_SESSIONS:DurableObjectNamespace<DurableAgentSession>}).NANOCODEX_SESSIONS;
 const agent=crypto.randomUUID(), owner=crypto.randomUUID();
 await runInDurableObject(sessions.getByName(agent),async(session,state)=>{
  const input={owner_id:owner,agent_id:agent,workflow_id:crypto.randomUUID(),message_id:'incoming',goal:'Reply about Friday only.',expires_at:1,message:{from:'venue@example.com',subject:'Friday',text:'Yes'}};
  await expect(session.resumeEmail(input)).rejects.toThrow('email_owner_forbidden');
  state.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,?,?, 'org','team',1,'https://nanocodex.example/','managed',?)`,agent,owner,Date.now());
  const current=(session as unknown as {env:Record<string,unknown>}).env;
  Object.defineProperty(session,'env',{value:{...current,NANOCODEX_EMAIL_ADMIN_ID:owner,NANOCODEX_EMAIL_OWNER_ID:owner}});
  expect((await session.resumeEmail(input)).state).toBe('failed'); // expired: no model or provider side effect
  await expect(session.resumeEmail({...input,owner_id:crypto.randomUUID()})).rejects.toThrow('email_owner_forbidden');
  await expect(session.resumeEmail({...input,agent_id:crypto.randomUUID()})).rejects.toThrow('email_owner_forbidden');
  Object.defineProperty(session,'env',{value:{...current,NANOCODEX_EMAIL_ADMIN_ID:crypto.randomUUID(),NANOCODEX_EMAIL_OWNER_ID:owner}});
  await expect(session.resumeEmail(input)).rejects.toThrow('email_owner_forbidden');
 });
});
