import { QueryClient } from "@tanstack/react-query";
import { EvalApiError } from "./evalApi.ts";

export function createEvalQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        gcTime: 30 * 60 * 1_000,
        refetchOnWindowFocus: false,
        retry: (failureCount, error) => {
          if (error instanceof EvalApiError
            && error.status >= 400
            && error.status < 500
            && error.status !== 408
            && error.status !== 425
            && error.status !== 429) {
            return false;
          }
          return failureCount < 2;
        },
      },
    },
  });
}
