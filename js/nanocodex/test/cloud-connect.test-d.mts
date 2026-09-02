import {
  Actions,
  Client,
  Dialog,
  Transport,
  type Connection,
  type ConnectorConnection,
  type ConnectorConnectionSelection,
  type ConnectorStatus,
} from "nanocodex/connect";
import { Voice } from "nanocodex/browser";

const client = Client.create({
  appId: "type-probe",
  dialog: Dialog.memory(),
  transport: Transport.mock(),
});
Dialog.popup({ target: "nanocodex-connect", features: "popup=yes" });

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

void standalone;
void decorated;

declare const connection: Connection;
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
