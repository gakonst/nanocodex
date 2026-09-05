import {
  QueryClient,
  QueryClientProvider,
} from "@tanstack/react-query";
import {
  useCallback,
  useEffect,
  useMemo,
} from "react";
import { formatUnits } from "viem";
import {
  WagmiProvider,
  useConnect,
  useConnection,
  useConnectors,
} from "wagmi";
import { tempo } from "wagmi/chains";
import { Hooks } from "wagmi/tempo";

import { PATH_USD } from "./tempo-policy";
import { wagmiConfig } from "./wagmi";

export type MppConnection = {
  start(): Promise<string | undefined>;
  balance?: string;
};

const queryClient = new QueryClient();

export function MppControls({
  onChange,
}: {
  onChange(connection: MppConnection | undefined): void;
}) {
  return (
    <WagmiProvider config={wagmiConfig} reconnectOnMount={false}>
      <QueryClientProvider client={queryClient}>
        <ConnectedMppControls onChange={onChange} />
      </QueryClientProvider>
    </WagmiProvider>
  );
}

function ConnectedMppControls({
  onChange,
}: {
  onChange(connection: MppConnection | undefined): void;
}) {
  const connection = useConnection();
  const connectors = useConnectors();
  const connect = useConnect();
  const connector = connectors[0];
  const connectAsync = connect.mutateAsync;
  const connectionAddress = connection.address;
  const connectionStatus = connection.status;
  const balance = Hooks.token.useGetBalance({
    account: connectionAddress,
    token: PATH_USD,
    query: {
      enabled: connectionStatus === "connected",
      refetchInterval: 5_000,
    },
  });
  const start = useCallback(async () => {
    if (!connector) throw new Error("Tempo Wallet connector is unavailable");
    if (connectionStatus === "connected") return connectionAddress;
    const connected = await connectAsync({ connector, chainId: tempo.id });
    return connected.accounts[0];
  }, [connectAsync, connectionAddress, connectionStatus, connector]);
  const formattedBalance = balance.data === undefined
    ? undefined
    : formatUnits(balance.data.amount, 6);
  const mppConnection = useMemo(
    () => ({ start, balance: formattedBalance }),
    [formattedBalance, start],
  );

  useEffect(() => {
    onChange(mppConnection);
    return () => onChange(undefined);
  }, [mppConnection, onChange]);

  return null;
}
