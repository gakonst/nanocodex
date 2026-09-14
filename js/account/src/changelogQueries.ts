import { queryOptions } from "@tanstack/react-query";
import { loadNightlyChangelog } from "./changelogData.ts";

export function changelogQueryOptions() {
  return queryOptions({
    queryKey: ["repository", "changelog"],
    queryFn: ({ signal }) => loadNightlyChangelog(fetch, undefined, signal),
    staleTime: 5 * 60_000,
  });
}
