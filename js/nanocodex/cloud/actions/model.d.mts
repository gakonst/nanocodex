import type { Transport } from "../../browser/Transport.mjs";
import type { Base } from "../Client.mjs";
import type { Connection, HostConnection } from "../types.mjs";

export declare namespace transport {
  type Options = Readonly<{ connection: Connection | HostConnection }>;
  type ReturnType = Transport;
  type ErrorType = Error;
}

/** Creates a local browser/WASM Responses transport backed by this Connect grant. */
export function transport(client: Base, options: transport.Options): transport.ReturnType;
