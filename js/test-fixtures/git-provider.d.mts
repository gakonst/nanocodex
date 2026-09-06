export function gitProvider(request: {
  url: string;
  method: string;
  headers: { get(name: string): string | null };
  arrayBuffer(): Promise<ArrayBuffer>;
}): Promise<Response | undefined>;
