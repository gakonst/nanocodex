export type BrowserCookieBinding = Readonly<{
  origin: string;
  profile_id: string;
  store_id: string;
}>;

export type BrowserCookieJarMetadata = BrowserCookieBinding & Readonly<{
  id: string;
  revision: number;
  cookie_count: number;
  updated_at: number;
}>;

export type BrowserCookieJarNames = BrowserCookieJarMetadata & Readonly<{
  cookie_names: readonly string[];
}>;

export function browserCookieBrokerPath(
  brokerUserId: string,
  jarId?: string,
  projection?: boolean | "materialize" | "names",
): string;
export function projectBrowserCookieJarList(
  value: unknown,
  binding: BrowserCookieBinding,
): { browser_cookie_jars: BrowserCookieJarMetadata[] };
export function projectBrowserCookieJarMetadata(value: unknown): BrowserCookieJarMetadata;
export function projectBrowserCookieJarMaterialization(
  value: unknown,
  jarId: string,
  binding: BrowserCookieBinding,
): Record<string, unknown>;
export function projectBrowserCookieJarNames(
  value: unknown,
  jarId: string,
  binding: BrowserCookieBinding,
): BrowserCookieJarNames;
export function projectBrowserCookieBrokerError(value: unknown): Readonly<{ error: string }>;
