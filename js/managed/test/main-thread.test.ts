import { describe, expect, it } from 'vitest';
import { mainThreadRequest, mainThreadTools } from '../src/main-thread';

describe('canonical Main protocol', () => {
  const agent = '11111111-1111-4111-8111-111111111111';
  it('ensures Main idempotently and leaves project membership untouched', async () => {
    let main: unknown;
    const keys: string[] = [];
    const host = { teamId: 'team-a',
      registry: async (path: string, init?: RequestInit) => {
        expect(path).toBe('/main-thread');
        if (init) main = JSON.parse(init.body as string);
        return Response.json(main ?? { error: 'not_found' }, { status: main ? 200 : 404 });
      },
      create: async (key: string) => { keys.push(key); return Response.json({ agent_id: agent }); },
    };
    for (let i = 0; i < 2; i++) expect(await (await mainThreadRequest(new Request('https://x/v1/main-thread', { method: 'PUT' }), host)).json()).toEqual({ agent_id: agent });
    expect(keys).toEqual(['canonical:team-a:main']);
  });
  it('does not recreate a tombstoned Main on lookup or ensure', async () => {
    const host = { teamId: 'team',
      registry: async () => Response.json({ error: 'main_thread_deleted' }, { status: 410 }),
      create: async () => { throw new Error('deleted canonical identity must remain reserved'); } };
    for (const method of ['GET', 'PUT']) {
      const response = await mainThreadRequest(new Request('https://x/v1/main-thread', { method }), host);
      expect(response.status).toBe(410);
      expect(await response.json()).toEqual({ error: 'main_thread_deleted' });
    }
  });
  it('reuses the canonical coordinator for rename and rejects reassignment', async () => {
    const data = [{ id: 'project', name: 'Old', coordinator_agent_id: agent }];
    const host = { teamId: 'team-a', create: async () => { throw new Error('must reuse'); },
      registry: async (_path: string, init?: RequestInit) => init ? Response.json(JSON.parse(init.body as string)) : Response.json({ data }) };
    const request = (body: unknown) => new Request('https://x/v1/projects/project', { method: 'PUT', body: JSON.stringify(body) });
    expect(await (await mainThreadRequest(request({ name: 'Renamed' }), host)).json()).toEqual({ name: 'Renamed', coordinator_agent_id: agent });
    expect((await mainThreadRequest(request({ name: 'Hijack', coordinator_agent_id: '22222222-2222-4222-8222-222222222222' }), host)).status).toBe(409);
  });
  it('rejects caller-selected scopes and invalid bodies before any effect', async () => {
    const host = { teamId: 'team', registry: async () => { throw new Error('unexpected'); }, create: async () => { throw new Error('unexpected'); } };
    for (const [path, body] of [['main-thread?team_id=other', '{}'], ['main-thread', '{"agent_id":"existing"}'], ['projects/p', '{"name":"Name","team_id":"other"}'], ['projects/%2F', '{}']]) {
      expect((await mainThreadRequest(new Request(`https://x/v1/${path}`, { method: 'PUT', body }), host)).status).toBe(400);
    }
  });
  it('freezes canonical create keys across retries and separates team identities', async () => {
    const keys: string[] = [];
    let available = false;
    const host = { teamId: 'team-a',
      registry: async (_path: string, init?: RequestInit) => init ? Response.json(JSON.parse(init.body as string)) : Response.json({ data: [] }),
      create: async (key: string) => { keys.push(key); return available ? Response.json({ agent_id: agent }) : new Response(null, { status: 503 }); },
    };
    const req = () => new Request('https://x/v1/projects/build', { method: 'PUT', body: JSON.stringify({ name: 'Build' }) });
    expect((await mainThreadRequest(req(), host)).status).toBe(503);
    available = true;
    expect((await mainThreadRequest(req(), host)).status).toBe(200);
    expect(keys).toEqual(['canonical:team-a:project:build', 'canonical:team-a:project:build']);
    await mainThreadRequest(req(), { ...host, teamId: 'team-b' });
    expect(keys.at(-1)).toBe('canonical:team-b:project:build');
  });

  it('registers an explicit existing root without creating a conversation', async () => {
    const host = { teamId: 'team',
      registry: async (_path: string, init?: RequestInit) => init ? Response.json(JSON.parse(init.body as string)) : Response.json({ data: [] }),
      create: async () => { throw new Error('must not create'); },
    };
    const body = { name: 'Existing', coordinator_agent_id: agent };
    expect(await (await mainThreadRequest(new Request('https://x/v1/projects/existing', { method: 'PUT', body: JSON.stringify(body) }), host)).json()).toEqual(body);
  });

  it('exposes explicit routing tools without changing project-thread tools', () => {
    const tools = mainThreadTools({ list: async () => [], read: async () => ({}), route: async () => ({}) });
    expect(tools.map(tool => tool.name)).toEqual(['list_projects', 'read_project', 'route_project']);
  });
});
