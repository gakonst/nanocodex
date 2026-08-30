import type { GenerateWebMcpManifestOptions } from "../webmcp/generator.mjs";

export type NanocodexNextWebMcpOptions = GenerateWebMcpManifestOptions;

/** Generate a review-first WebMCP manifest on Next.js startup and build. */
export function withWebMcp<const arguments_ extends readonly unknown[], const config extends object>(
  nextConfig: (...arguments_: arguments_) => config | Promise<config>,
  options?: NanocodexNextWebMcpOptions,
): (...arguments_: arguments_) => Promise<config>;

/** Generate a review-first WebMCP manifest on Next.js startup and build. */
export function withWebMcp<const config extends object = Readonly<Record<string, unknown>>>(
  nextConfig?: config,
  options?: NanocodexNextWebMcpOptions,
): (phase: string, context: unknown) => Promise<config>;
