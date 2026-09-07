import type { NamedTool } from "./types.mjs";

export type XRequest = Readonly<{
  action: "post" | "profile" | "search" | "followers" | "following";
  url?: string;
  handle?: string;
  q?: string;
  feed?: "latest" | "top";
  cursor?: string;
  page?: number;
  limit?: number;
  thread?: string;
  context?: "full" | "thread";
  replies?: "top" | "recent" | "off";
  userinfo?: "off" | "author" | "all";
  full?: boolean;
  nocache?: boolean;
}>;

export const X_API: Readonly<{
  id: "x";
  name: string;
  tool: "browseX";
  authentication: "none";
  description: string;
}>;
export function parseXRequest(input: unknown): XRequest;
export function browseX(options: {
  fetch: (input: string | URL, init?: RequestInit) => Promise<Response>;
}): NamedTool;
