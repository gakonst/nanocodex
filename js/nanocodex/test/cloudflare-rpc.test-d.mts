import { consumeRpcData } from "nanocodex/cloudflare/rpc";
const record = consumeRpcData({ status: 200, catalog: { connectors: {}, mcp_connections: [] } });
const status: number = record.status;
const optional: { id: string } | undefined = consumeRpcData(undefined as { id: string } | undefined);
const primitive: string = consumeRpcData("synthetic");
void [status, optional, primitive];

// Workerd object results carry an owner; detached data does not.
declare const owned: { status: number; catalog: { connectors: object } } & Disposable;
const detached = consumeRpcData(owned);
const detachedStatus: number = detached.status;
// @ts-expect-error the detached record has no RPC owner to dispose
detached[Symbol.dispose]();
declare const maybeOwned: ({ id: string } & Disposable) | undefined;
const maybeData: { id: string } | undefined = consumeRpcData(maybeOwned);
void [detachedStatus, maybeData];
