import { memo } from "react";
import { generatedOutputUrl, type GeneratedOutput } from "nanocodex-react/agent";
import { Streamdown } from "streamdown";

/** User-facing code output, independent of the activity disclosure's state. */
export const GeneratedOutputView = memo(function GeneratedOutputView({ items }: {
  items: readonly GeneratedOutput[];
}) {
  if (!items.length) return null;
  return <div className="agent-generated-output" aria-label="Generated output">
    {items.map((item, index) => item.kind === "text" ? (
      <div className="agent-generated-text" key={`text:${item.text}`}>
        <Streamdown mode="static" skipHtml linkSafety={{ enabled: true }}
          controls={{ code: { copy: true, download: false }, table: false, mermaid: false }}>
          {item.text}
        </Streamdown>
      </div>
    ) : (
      <GeneratedMedia key={`${item.kind}:${item.url}`} item={item} index={index} />
    ))}
  </div>;
});

const GeneratedMedia = memo(function GeneratedMedia({ item, index }: {
  item: Exclude<GeneratedOutput, { kind: "text" }>;
  index: number;
}) {
  const url = generatedOutputUrl(item.url, item.kind);
  const name = item.name || defaultName(item, index);
  if (!url) return <p className="agent-generated-unavailable">{name} — preview unavailable</p>;
  return <figure className={`agent-generated-media is-${item.kind}`}>
    {item.kind === "image" ? <img src={url} alt={name} loading="lazy" decoding="async" referrerPolicy="no-referrer" /> : null}
    {item.kind === "audio" ? <audio src={url} controls preload="none" aria-label={name} /> : null}
    {item.kind === "video" ? <video src={url} controls playsInline preload="metadata" aria-label={name} /> : null}
    <figcaption>
      <span>{name}</span>
      <a href={url} download={name} {...(!url.startsWith("data:") ? { target: "_blank" } : {})}
        rel="noopener noreferrer" aria-label={`Download ${name}`}>
        {item.kind === "file" ? "Open / download" : "Download"}
      </a>
    </figcaption>
  </figure>;
});

function defaultName(item: Exclude<GeneratedOutput, { kind: "text" }>, index: number): string {
  if (!item.url.startsWith("data:")) {
    try {
      const file = new URL(item.url).pathname.split("/").at(-1);
      if (file?.includes(".")) return decodeURIComponent(file).slice(0, 240);
    } catch { /* Fall back to the declared media type. */ }
  }
  const extensions: Record<string, string> = {
    "image/png": "png", "image/jpeg": "jpg", "image/webp": "webp", "image/gif": "gif", "image/svg+xml": "svg",
    "audio/mpeg": "mp3", "audio/wav": "wav", "audio/mp4": "m4a", "audio/ogg": "ogg",
    "video/mp4": "mp4", "video/webm": "webm", "application/pdf": "pdf", "application/json": "json",
    "text/plain": "txt", "text/markdown": "md", "text/html": "html", "text/csv": "csv",
  };
  const extension = item.mimeType ? extensions[item.mimeType] : undefined;
  return `Generated ${item.kind} ${index + 1}${extension ? `.${extension}` : ""}`;
}
