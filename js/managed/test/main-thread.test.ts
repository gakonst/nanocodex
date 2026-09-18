import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from 'vitest';
import { canonicalRoleResponse, mainThreadRequest, mainThreadTools, retainMainCoordinatorCreation, retainMainRoute } from '../src/main-thread';

describe('canonical Main protocol', () => {
  const agent = '11111111-1111-4111-8111-111111111111';
  it('ensures Main idempotently and leaves project membership untouched', async () => {
    let main: unknown;
    const keys: string[] = [];
    const host = { teamId: 'team-a',
      registry: async (path: string, init?: RequestInit) => {
        if (path === '/canonical-generations/main') return Response.json({ generation: 0 });
        expect(path).toBe('/main-thread');
        if (init) main = JSON.parse(init.body as string);
        return Response.json(main ?? { error: 'not_found' }, { status: main ? 200 : 404 });
      },
      create: async (key: string) => { keys.push(key); return Response.json({ agent_id: agent }); },
    };
    for (let i = 0; i < 2; i++) expect(await (await mainThreadRequest(new Request('https://x/v1/main-thread', { method: 'PUT' }), host)).json()).toEqual({ agent_id: agent });
    expect(keys).toEqual(['canonical:team-a:main']);
  });
  it('propagates unresolved legacy tombstones instead of treating them as absent registrations', async () => {
    const host = { teamId: 'team',
      registry: async () => Response.json({ error: 'main_thread_deleted' }, { status: 410 }),
      create: async () => { throw new Error('a 410 response is not an absent registration'); } };
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
      registry: async (path: string, init?: RequestInit) => path.startsWith("/canonical-generations/") ? Response.json({ generation: 0 }) : init ? Response.json(JSON.parse(init.body as string)) : Response.json({ data: [] }),
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
      registry: async (path: string, init?: RequestInit) => path.startsWith("/canonical-generations/") ? Response.json({ generation: 0 }) : init ? Response.json(JSON.parse(init.body as string)) : Response.json({ data: [] }),
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


describe('canonical role availability boundary', () => {
  it.each([400, 401, 403, 410, 429, 500, 503])('does not fall back to project spawning for registry status %s', async status => {
    await expect(canonicalRoleResponse(Response.json({ error: 'unavailable' }, { status }))).rejects.toThrow('registry unavailable');
  });
  it('permits ordinary conversation fallback only for absent registration', async () => {
    expect(await canonicalRoleResponse(new Response(null, { status: 404 }))).toEqual({ role: 'conversation' });
    expect(await canonicalRoleResponse(Response.json({ role: 'main' }))).toEqual({ role: 'main' });
  });
});


it('freezes coordinator creation per project across failed creation, settings changes, and route IDs', async () => {
  const users = (env as unknown as { NANOCODEX_USERS: DurableObjectNamespace }).NANOCODEX_USERS;
  await runInDurableObject(users.getByName(crypto.randomUUID()), async (_account, state) => {
    const initial = JSON.stringify({ settings: { model: 'original' }, configuration: { instructions: 'Original policy', multi_agent: { enabled: true } } });
    const changed = JSON.stringify({ settings: { model: 'changed' }, configuration: { instructions: 'Changed policy' } });
    const input = { id: 'first', project_id: 'research', name: 'Research', input: 'Do the work' };
    const bodies: string[] = [], keys: string[] = [];
    const projects: Array<{ id: string; name: string; coordinator_agent_id: string }> = [];
    const route = async (id: string, creation: string) => {
      retainMainRoute(state.storage, { ...input, id });
      const snapshot = retainMainCoordinatorCreation(state.storage, input.project_id, creation);
      return mainThreadRequest(new Request('https://test/v1/projects/research', { method: 'PUT', body: JSON.stringify({ name: 'Research' }) }), {
        teamId: 'team',
        registry: async (path, init) => {
          if (path === '/canonical-generations/projects/research') return Response.json({ generation: 0 });
          if (!init) return Response.json({ data: projects });
          const body = JSON.parse(init.body as string);
          projects[0] = { id: 'research', ...body };
          return Response.json(projects[0]);
        },
        create: async key => {
          keys.push(key); bodies.push(snapshot);
          return bodies.length === 1 ? new Response(null, { status: 503 }) : Response.json({ agent_id: '11111111-1111-4111-8111-111111111111' });
        },
      });
    };
    expect((await route('first', initial)).status).toBe(503);
    expect((await route('first', changed)).status).toBe(200);
    expect(bodies).toEqual([initial, initial]);
    expect(keys).toEqual(['canonical:team:project:research', 'canonical:team:project:research']);
    expect((await route('followup', changed)).status).toBe(200);
    expect(bodies).toHaveLength(2);
    expect(retainMainCoordinatorCreation(state.storage, 'research', changed)).toBe(initial);
    expect(retainMainCoordinatorCreation(state.storage, 'other', changed)).toBe(changed);
    expect(JSON.parse(initial)).not.toHaveProperty('capabilities');
  });
});
