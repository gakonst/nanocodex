import { env } from 'cloudflare:workers';
import { SELF } from 'cloudflare:test';
import { beforeEach, describe, expect, it } from 'vitest';
import type { EgressEnv } from '../src/egress';
import { RoutedUserBroker, resetBrokerLocations } from '../src/broker-router';
import { homeBrokerName, parseBrokerName, type PlacementState } from '../src/broker-placement';

// Real workerd broker objects, sealed storage and RPC. The re-home cooldown is
// shortened to 4 s by the test binding (honoured only in test environments).
const worker = env as unknown as EgressEnv;
const COOLDOWN_MS = 4_000;
const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));
function stub(name: string) {
  const parsed = parseBrokerName(name);
  return worker.USER_CREDENTIALS.getByName(name, parsed?.kind === 'home' ? { locationHint: parsed.region } : undefined);
}
const where = (user: string, region?: string) => region ? homeBrokerName(region, user) : user;
const placement = async (user: string, region?: string) => await stub(where(user, region)).placementState() as PlacementState;
async function control(user: string, method: string, apiKey?: string) {
  const response = await SELF.fetch(`https://broker.internal/users/${user}/credentials/openai`, { method,
    ...(apiKey === undefined ? {} : { headers: { 'content-type': 'application/json' }, body: JSON.stringify({ api_key: apiKey }) }) });
  await response.body?.cancel();
  return response.status;
}
async function read(user: string, region?: string) {
  const result = await new RoutedUserBroker(worker, user, region ? { claim: region } : {}).resolveModelCredential(false);
  return { status: result.status, secret: result.credential?.secret ?? null };
}
async function direct(user: string, region?: string): Promise<string> {
  try { await stub(where(user, region)).resolveModelCredential(false); return 'served'; }
  catch (error) { return error instanceof Error ? error.message : String(error); }
}

describe('canonical credential broker placement', { timeout: 30_000 }, () => {
  beforeEach(() => resetBrokerLocations());

  it('adopts sealed legacy state next to the user and tombstones the legacy object', async () => {
    const user = `placement-${crypto.randomUUID()}`;
    const other = `placement-other-${crypto.randomUUID()}`;
    expect(await control(user, 'PUT', 'sk-legacy-a')).toBe(204);
    expect(await control(other, 'PUT', 'sk-other')).toBe(204);
    expect((await placement(user)).state).toBe('active');
    expect((await placement(user, 'wnam')).state).toBe('empty');

    expect(await read(user, 'wnam')).toEqual({ status: 200, secret: 'sk-legacy-a' });
    expect((await placement(user, 'wnam')).state).toBe('active');
    expect(await placement(user)).toMatchObject({ state: 'moved', target: homeBrokerName('wnam', user) });

    // Fail closed: the tombstone and unadopted homes refuse before any work.
    expect(await direct(user)).toMatch(/broker_moved/);
    expect(await direct(user, 'apac')).toMatch(/broker_moved/);
    const moved = await stub(user).fetch('https://credentials.internal/v1/status');
    expect(moved.status).toBe(421);
    await moved.body?.cancel();
    // Another user's home can never receive these rows.
    expect(await stub(homeBrokerName('wnam', user)).releaseTo(homeBrokerName('weur', other))).toEqual({ status: 'invalid' });

    // Regionless control and reads follow the directory to the home.
    expect(await control(user, 'PUT', 'sk-home-b')).toBe(204);
    resetBrokerLocations();
    expect(await read(user)).toEqual({ status: 200, secret: 'sk-home-b' });
    expect(await read(user, 'wnam')).toEqual({ status: 200, secret: 'sk-home-b' });
    expect(await read(other)).toEqual({ status: 200, secret: 'sk-other' });
  });

  it('redirects inside the cooldown, re-homes after it, and never serves a stale tenure', async () => {
    const user = `rehome-${crypto.randomUUID()}`;
    expect(await control(user, 'PUT', 'sk-a')).toBe(204);
    expect(await read(user, 'wnam')).toEqual({ status: 200, secret: 'sk-a' });

    // Inside the cooldown another region is redirected, not re-homed.
    expect(await read(user, 'weur')).toEqual({ status: 200, secret: 'sk-a' });
    expect((await placement(user, 'weur')).state).toBe('empty');
    expect((await placement(user, 'wnam')).state).toBe('active');

    await sleep(COOLDOWN_MS + 200);
    resetBrokerLocations();
    expect(await read(user, 'weur')).toEqual({ status: 200, secret: 'sk-a' });
    expect((await placement(user, 'weur')).state).toBe('active');
    expect(await placement(user, 'wnam')).toMatchObject({ state: 'moved', target: homeBrokerName('weur', user) });
    await sleep(300);
    // Legacy directory is repointed (compare-and-set) and the confirmed
    // tombstone never re-exports.
    expect(await placement(user)).toMatchObject({ state: 'moved', target: homeBrokerName('weur', user) });
    expect(await stub(homeBrokerName('wnam', user)).releaseTo(homeBrokerName('weur', user)))
      .toEqual({ status: 'moved', target: homeBrokerName('weur', user) });

    // Mutate in the new home; returning home must serve the newest state.
    expect(await control(user, 'PUT', 'sk-c')).toBe(204);
    await sleep(COOLDOWN_MS + 200);
    resetBrokerLocations();
    expect(await read(user, 'wnam')).toEqual({ status: 200, secret: 'sk-c' });
    expect((await placement(user, 'wnam')).state).toBe('active');
    expect((await placement(user, 'weur')).state).toBe('moved');

    // A stale isolate hint (weur) is refused and re-located, never served.
    expect(await direct(user, 'weur')).toMatch(/broker_moved/);
    expect(await read(user)).toEqual({ status: 200, secret: 'sk-c' });

    // Revocation is final everywhere.
    expect(await control(user, 'DELETE')).toBe(204);
    for (const region of ['wnam', 'weur', undefined]) {
      resetBrokerLocations();
      const result = await read(user, region);
      expect(result.secret).toBeNull();
    }
  });

  it('resumes an interrupted adoption idempotently from the tombstone', async () => {
    const user = `resume-${crypto.randomUUID()}`;
    expect(await control(user, 'PUT', 'sk-resume')).toBe(204);
    // The predecessor exported (and tombstoned itself) but the home never committed.
    const exported = await stub(user).releaseTo(homeBrokerName('enam', user));
    expect(exported.status).toBe('exported');
    expect(await direct(user)).toMatch(/broker_moved/);
    // Re-export to the same successor is allowed; a different home is redirected.
    expect((await stub(user).releaseTo(homeBrokerName('enam', user))).status).toBe('exported');
    expect(await stub(user).releaseTo(homeBrokerName('weur', user)))
      .toEqual({ status: 'moved', target: homeBrokerName('enam', user) });
    expect(await read(user, 'enam')).toEqual({ status: 200, secret: 'sk-resume' });
    expect((await placement(user, 'enam')).state).toBe('active');
  });
});
