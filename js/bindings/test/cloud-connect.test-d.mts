import { Actions, Client, Dialog, Identity, Transport, type Connection } from "nanocodex/connect";
import { Session } from "nanocodex/connect/server";
import { Voice } from "nanocodex/browser";

const client = Client.create({
  appId: "type-probe",
  appOrigin: "https://app.example.com",
  dialog: Dialog.memory(),
  identity: Identity.host(),
  transport: Transport.mock(),
});
Dialog.popup({ target: "nanocodex-connect", features: "popup=yes" });

const standalone: Promise<Connection> = Actions.connection.connect(client, {
  capabilities: {
    auth: { resources: ["urn:nanocodex:connector:github:repo-read"] },
    cloudAccounts: { github: true, gmail: true, gdrive: true, x: true, chatgpt: true },
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
connector satisfies "github" | "gmail" | "gdrive" | "x" | "chatgpt" | undefined;

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

const sessions = Session.create({
  appId: "type-probe",
  appOrigin: "https://app.example.com",
  secret: "project-secret-that-is-long-enough-123",
});
const sessionRoute: (request: Request) => Promise<Response> = sessions.handler({
  async authenticate(request) {
    return request.headers.has("cookie") ? { subject: "user-123" } : undefined;
  },
});
void sessionRoute;
