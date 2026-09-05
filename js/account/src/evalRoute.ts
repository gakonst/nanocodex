export type EvalRoute =
  | { kind: "overview" }
  | { kind: "workset"; worksetId: string }
  | { kind: "task"; worksetId: string; taskId: string }
  | { kind: "unknown" };

function decodeSegment(value: string) {
  try {
    return decodeURIComponent(value);
  } catch {
    return null;
  }
}

export function evalRouteFromPath(pathname: string): EvalRoute {
  const segments = pathname.split("/").filter(Boolean);
  if (segments.length === 1 && segments[0] === "evals") {
    return { kind: "overview" };
  }
  if (segments.length === 3 && segments[0] === "evals" && segments[1] === "worksets") {
    const worksetId = decodeSegment(segments[2]!);
    return worksetId ? { kind: "workset", worksetId } : { kind: "unknown" };
  }
  if (
    segments.length === 5 &&
    segments[0] === "evals" &&
    segments[1] === "worksets" &&
    segments[3] === "tasks"
  ) {
    const worksetId = decodeSegment(segments[2]!);
    const taskId = decodeSegment(segments[4]!);
    return worksetId && taskId
      ? { kind: "task", worksetId, taskId }
      : { kind: "unknown" };
  }
  return { kind: "unknown" };
}
