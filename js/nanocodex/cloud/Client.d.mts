import type { ConnectActions } from "./Decorator.mjs";
import type { Instance as DialogInstance, Dialog } from "./Dialog.mjs";
import type { Instance as PrincipalInstance, Principal } from "./Principal.mjs";
import type { Request, Transport } from "./Transport.mjs";
import type { Auth, AuthorizeAccessKey } from "./actions/connection.mjs";
import type { HostConnection } from "./types.mjs";

export type Provider = Readonly<{
  request(request: Readonly<{
    method: string;
    params?: readonly unknown[] | undefined;
    context?: Readonly<Record<string, unknown>> | undefined;
  }>): Promise<unknown>;
  /** Internal lifecycle reset used after account logout. */
  reset?(): Promise<void>;
  store?: Readonly<{
    getState?(): Readonly<{
      accounts?: readonly Readonly<{ address: string }>[] | undefined;
      activeAccount?: number | undefined;
      accessKeys?: readonly Readonly<{
        access: string;
        address: string;
        chainId: number;
        expiry: number;
      }>[] | undefined;
    }>;
  }> | undefined;
}>;

export type Base<principal extends PrincipalInstance | undefined = PrincipalInstance | undefined> = Readonly<{
  appId: string;
  appOrigin: string | undefined;
  accessKey: Readonly<{ authorize?: AuthorizeAccessKey | undefined }> | undefined;
  auth: Auth | undefined;
  dialog: DialogInstance;
  key: string;
  name: string;
  principal: principal;
  provider: Provider;
  type: "connect";
  uid: string;
  transport: Readonly<{ key: string; name: string; type: string; baseUrl: string }>;
  fetch(input: RequestInfo | URL, init?: RequestInit | undefined): Promise<Response>;
  request(request: Request): Promise<unknown>;
}>;

export type Client<
  extension extends object = ConnectActions,
  principal extends PrincipalInstance | undefined = undefined,
> = Base<principal> & extension & {
  extend<next extends object>(decorator: (client: Client<extension, principal>) => next): Client<extension & next, principal>;
};

type BaseParameters = Readonly<{
  appId: string;
  /** Exact browser origin bound into approvals and grants. Defaults to location.origin. */
  appOrigin?: string | undefined;
  /** Accounts-compatible default SIWE round-trip configuration. */
  auth?: Auth | undefined;
  /** Accounts-compatible default access-key authorization policy. */
  accessKey?: Readonly<{ authorize?: AuthorizeAccessKey | undefined }> | undefined;
  dialog?: Dialog | undefined;
  key?: string | undefined;
  name?: string | undefined;
  /** Advanced override for the remote wallet provider that owns the access key. */
  provider?: Provider | undefined;
  /** App-scoped grant session persistence. Defaults to browser localStorage. */
  session?: Pick<Storage, "getItem" | "setItem" | "removeItem"> | false | undefined;
  transport?: Transport | undefined;
}>;

export type Parameters<principal extends Principal | undefined = undefined> = BaseParameters & (
  principal extends Principal
    ? Readonly<{
      /** Existing host login used only for hosted, principal-scoped authorization. */
      principal: principal;
    }>
    : Readonly<{ principal?: undefined }>
);

export function create(parameters: Parameters<Principal<"host">>): Client<ConnectActions<HostConnection>, PrincipalInstance>;
export function create(parameters: Parameters): Client;
export function create(parameters: Parameters<Principal>): Client<ConnectActions<HostConnection>, PrincipalInstance>;
