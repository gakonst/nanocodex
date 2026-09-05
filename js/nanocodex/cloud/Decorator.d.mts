import type { logout } from "./actions/account.mjs";
import type { create as createAgent } from "./actions/agent.mjs";
import type { connect, disconnect, reconnect } from "./actions/connection.mjs";
import type { revoke } from "./actions/grant.mjs";
import type { fund, getConfig } from "./actions/machineUsd.mjs";
import type { transport as modelTransport } from "./actions/model.mjs";
import type { charge, getBalance } from "./actions/mpp.mjs";
import type { Client } from "./Client.mjs";
import type { Connection, HostConnection } from "./types.mjs";

export type ConnectActions<connection extends Connection | HostConnection = Connection> = {
  account: { logout(): logout.ReturnType };
  agent: { create(options: createAgent.Options): Promise<createAgent.ReturnType> };
  connection: {
    connect(options: connect.Options): Promise<connection>;
    disconnect(options?: disconnect.Options | undefined): disconnect.ReturnType;
    reconnect(options?: reconnect.Options | undefined): Promise<connection | undefined>;
  };
  grant: { revoke(options: revoke.Options): revoke.ReturnType };
  machineUsd: {
    fund(options: fund.Options): fund.ReturnType;
    getConfig(options?: getConfig.Options | undefined): getConfig.ReturnType;
  };
  model: { transport(options: modelTransport.Options): modelTransport.ReturnType };
  mpp: {
    charge(options: charge.Options): charge.ReturnType;
    getBalance(options: getBalance.Options): getBalance.ReturnType;
  };
};

export function connectActions(): (client: Client) => ConnectActions;
