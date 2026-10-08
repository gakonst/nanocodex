// Test-only entry: production egress + broker + snapshot classes, plus a
// fault-injection subclass whose invalidate consults the outbound fixture
// and whose canonical grant reply passes through an outbound delay gate.
import Egress from "../../src/egress";
import { snapshotStub, UserCredentialSnapshot } from "../../src/credential-snapshot";
export * from "../../src/egress";

type SnapshotEnv = ConstructorParameters<typeof UserCredentialSnapshot>[1];

/** The real broker grant (lease registered, durable) whose reply is held by
 * the test until it releases `https://fault.fixture/grant/<region>`. */
function delayedGrants(env: SnapshotEnv): SnapshotEnv {
  const brokers = env.USER_CREDENTIALS;
  return { ...env, USER_CREDENTIALS: { getByName(name: string) {
    const stub = brokers.getByName(name);
    return { async grantModelCredentialLease(owner: string, region: string) {
      const grant = await stub.grantModelCredentialLease(owner, region);
      await fetch(`https://fault.fixture/grant/${region}`);
      return grant;
    } };
  } } as unknown as SnapshotEnv["USER_CREDENTIALS"] };
}

export class FaultInjectedSnapshot extends UserCredentialSnapshot {
  constructor(state: DurableObjectState, env: SnapshotEnv) {
    super(state, delayedGrants(env));
  }

  override async invalidate(owner: string, region: string, epoch: number): Promise<boolean> {
    const fault = await fetch(`https://fault.fixture/invalidate/${region}`);
    if (fault.status !== 200) throw new Error("injected invalidation failure");
    return super.invalidate(owner, region, epoch);
  }
}

export default class TestEgress extends Egress {
  override async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    const match = /^\/__test\/resolve\/([a-z]+)\/([^/]+)$/.exec(url.pathname);
    if (!match) return super.fetch(request);
    const [, region, owner] = match;
    const stub = snapshotStub(this.env as never, owner!, region!);
    if (!stub) return Response.json({ status: 0 });
    const result = await stub.resolve(owner!, region!);
    return Response.json({ status: result.status, source: result.source, secret: result.credential?.secret ?? null });
  }
}
