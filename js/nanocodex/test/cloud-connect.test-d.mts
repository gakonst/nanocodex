import {
  Actions,
  Client,
  Dialog,
  Principal,
  Transport,
  type Connection,
  type ConnectorConnection,
  type ConnectorConnectionSelection,
  type ConnectorStatus,
  type HostConnection,
} from "nanocodex/connect";
import { HostPrincipal } from "nanocodex/connect/server";
import { Voice } from "nanocodex/browser";

const client = Client.create({
  appId: "type-probe",
  dialog: Dialog.memory(),
  transport: Transport.mock(),
});
const principalClient = Client.create({
  appId: "type-probe",
  appOrigin: "https://app.example.com",
  principal: Principal.host(),
  dialog: Dialog.memory(),
  transport: Transport.mock(),
});
const principalConnection = await principalClient.connection.connect({
  authorization: "hosted",
  capabilities: { cloudAccounts: { github: true } },
});
principalConnection satisfies HostConnection;
principalConnection.principal.kind satisfies "host";
principalConnection.principal.id satisfies string;
principalConnection.accountAddress satisfies undefined;
principalConnection.accessKey satisfies undefined;
principalConnection.mpp satisfies undefined;
principalClient.principal.create satisfies Function;
const principalAgent = principalClient.agent.create({ connection: principalConnection });
const principalModelTransport = principalClient.model.transport({ connection: principalConnection });
const standalonePrincipalConnection = await Actions.connection.connect(principalClient, {
  authorization: "hosted",
});
standalonePrincipalConnection.principal.kind satisfies "host";
standalonePrincipalConnection.accountAddress satisfies undefined;
Actions.agent.create(principalClient, { connection: standalonePrincipalConnection });
Actions.model.transport(principalClient, { connection: standalonePrincipalConnection });
const principalReconnect = await principalClient.connection.reconnect();
if (principalReconnect) {
  principalReconnect satisfies HostConnection;
  principalReconnect.principal.kind satisfies "host";
  principalReconnect.accountAddress satisfies undefined;
}
const widenedPrincipal: import("../cloud/Principal.mjs").Principal = Principal.host();
Client.create({
  appId: "widened-principal-type-probe",
  appOrigin: "https://app.example.com",
  principal: widenedPrincipal,
  dialog: Dialog.memory(),
  transport: Transport.mock(),
});
Dialog.popup({ target: "nanocodex-connect", features: "popup=yes" });

const explicitClient: Client.Client = Client.create({
  appId: "explicit-client-type-probe",
  dialog: Dialog.memory(),
  transport: Transport.mock(),
});
const explicitClientConnection = await explicitClient.connection.connect({});
explicitClientConnection.accountAddress satisfies `0x${string}`;

const parameters: Client.Parameters = {
  appId: "parameters-type-probe",
  dialog: Dialog.memory(),
  transport: Transport.mock(),
};
const parametersClient = Client.create(parameters);
const parametersConnection = await parametersClient.connection.connect({});
parametersConnection.accountAddress satisfies `0x${string}`;
void principalAgent;
void principalModelTransport;

const standalone: Promise<Connection> = Actions.connection.connect(client, {
  capabilities: {
    auth: { resources: ["urn:nanocodex:connector:github:repo-read"] },
    cloudAccounts: {
      github: true,
      gmail: true,
      gdrive: true,
      gcalendar: true,
      gtasks: true,
      gdocs: true,
      gsheets: true,
      gslides: true,
      gcontacts: true,
      slack: true,
      x: true,
      chatgpt: true,
    },
  },
});
const standaloneWalletConnection = await Actions.connection.connect(client, {});
standaloneWalletConnection.accountAddress satisfies `0x${string}`;
standaloneWalletConnection.principal satisfies undefined;
const standaloneWalletReconnect = await Actions.connection.reconnect(client);
if (standaloneWalletReconnect) {
  standaloneWalletReconnect.accountAddress satisfies `0x${string}`;
}
const decorated: Promise<Connection> = client.connection.connect({
  capabilities: {
    authorizeAccessKey: {
      expiry: Math.floor(Date.now() / 1_000) + 3_600,
      limits: [{
        token: "0x20c0000000000000000000000000000000000001",
        limit: 10_000_000n,
        period: 86_400,
      }],
    },
  },
});
const walletConnection = await client.connection.connect({});
walletConnection.accountAddress satisfies `0x${string}`;
walletConnection.principal satisfies undefined;
client.principal satisfies undefined;
const walletReconnect = await client.connection.reconnect();
if (walletReconnect) {
  walletReconnect.accountAddress satisfies `0x${string}`;
  walletReconnect.principal satisfies undefined;
}

void standalone;
void decorated;

declare const connection: Connection;
connection.accountAddress satisfies `0x${string}`;
const connector = connection.grant.connectors[0];
connector satisfies "github" | "gmail" | "gdrive" | "gcalendar" | "gtasks" | "gdocs" | "gsheets" | "gslides" | "gcontacts" | "slack" | "x" | "chatgpt" | undefined;
const connectorConnections: ConnectorConnectionSelection | undefined = connection.grant.connectorConnections;
const connectorStatus: ConnectorStatus = {
  connected: true,
  connections: [{
    id: "a".repeat(43),
    label: "Work account",
    accountId: "provider-account",
    capabilities: ["gmail", "gdrive"],
  } satisfies ConnectorConnection],
};
void connectorConnections;
void connectorStatus;

client.connection.connect({
  capabilities: {
    // @ts-expect-error Cloud account capabilities accept exact true values only.
    cloudAccounts: { github: false },
  },
});
const agent = client.agent.create({ connection });
const voice = Voice.create(await agent);
const agentTurn = (await agent).turn.prompt({ input: "Review my pull requests" });
const agentResult = agentTurn.result();
void agentResult;
void voice;
const localModelTransport = client.model.transport({ connection });
localModelTransport satisfies import("../browser/Transport.mjs").Transport;
client.machineUsd.fund({
  accountAddress: connection.accountAddress,
  grantId: connection.grant.id,
  usdAmountCents: 500,
});
client.mpp.charge({
  amount: 100_000n,
  grantId: connection.grant.id,
  origin: "https://models.example",
});

const hostPrincipals = HostPrincipal.create({
  appId: "type-probe",
  appOrigin: "https://app.example.com",
  secret: "project-secret-that-is-long-enough-123",
});
const exchange = hostPrincipals.create({
  issuer: "example-auth",
  tenant: "example-tenant",
  subject: "user-123",
  sessionId: "session-456",
  resources: ["urn:nanocodex:app:type-probe"],
  expiresIn: 60,
});
const sessionRoute: (request: Request) => Promise<Response> = hostPrincipals.handler({
  authenticate(request) {
    return request.headers.has("cookie")
      ? {
          issuer: "example-auth",
          tenant: "example-tenant",
          subject: "user-123",
          sessionId: "session-456",
        }
      : undefined;
  },
});
await hostPrincipals.revoke({
  issuer: "example-auth",
  tenant: "example-tenant",
  subject: "user-123",
  sessionId: "session-456",
});
void exchange;
void sessionRoute;
