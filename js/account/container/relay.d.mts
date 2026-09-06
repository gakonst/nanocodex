import type { Server } from "node:http";

export function startRelay(options?: {
  host?: string;
  port?: number;
  upstreamOrigin?: string;
}): Server;

export function responseStatus(header: Buffer): number;
