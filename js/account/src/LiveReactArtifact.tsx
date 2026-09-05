import { useEffect, useRef } from "react";
import type { ArtifactDocument } from "nanocodex/tools/artifact";

export function LiveReactArtifact({
  artifact,
  onAction,
}: {
  artifact: ArtifactDocument;
  onAction(prompt: string): void;
}) {
  const frame = useRef<HTMLIFrameElement>(null);

  useEffect(() => {
    const receive = (event: MessageEvent) => {
      if (event.source !== frame.current?.contentWindow) return;
      const value = asRecord(event.data);
      if (value?.type === "artifact-runtime-ready") {
        postArtifact(frame.current, artifact);
        return;
      }
      if (value?.type !== "artifact-action" || value.artifactId !== artifact.id) return;
      if (typeof value.prompt === "string" && value.prompt.trim()) onAction(value.prompt);
    };
    window.addEventListener("message", receive);
    return () => window.removeEventListener("message", receive);
  }, [artifact.id, onAction]);

  return (
    <iframe
      key={`${artifact.id}:${artifact.updatedAt}`}
      ref={frame}
      className="live-react-artifact"
      src="/artifact-runtime?embedded=1"
      sandbox="allow-scripts"
      title={artifact.title}
      onLoad={() => postArtifact(frame.current, artifact)}
    />
  );
}

function postArtifact(frame: HTMLIFrameElement | null, artifact: ArtifactDocument): void {
  frame?.contentWindow?.postMessage({
    type: "render-artifact",
    artifactId: artifact.id,
    source: artifact.source,
  }, "*");
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}
