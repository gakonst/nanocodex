import { memo, useEffect, useMemo, useRef, useState, type ComponentProps } from "react";
import { Streamdown, extractTableDataFromElement, tableDataToTSV, tableDataToCSV } from "streamdown";
import { lazyCode as code } from "./lazyCode.js";
import { lazyMermaid as mermaid } from "./lazyMermaid.js";
import { HtmlPreview } from "./HtmlPreview.js";
import { splitHtmlFences } from "./htmlDocument.js";
import { MarkdownLink } from "./LinkedFile.js";

const plugins = { code, mermaid };
const controls = {
  code: { copy: true, download: false },
  table: { copy: true, download: true, fullscreen: false },
  mermaid: { copy: true, download: false, fullscreen: false, panZoom: false },
} as const;
const linkSafety = { enabled: true };
const diagramOptions = { config: { securityLevel: "strict" as const, startOnLoad: false } };

function MarkdownInput({ node: _node, ref: _ref, ...props }: ComponentProps<"input"> & { node?: unknown }) {
  return <input {...props} aria-label={props["aria-label"] ?? (props.type === "checkbox" ? "Checklist item" : undefined)} />;
}

function MarkdownImage({ node: _node, ref: _ref, ...props }: ComponentProps<"img"> & { node?: unknown }) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [props.src]);
  if (failed) return <span className="agent-rich-image-unavailable">Image unavailable{props.alt ? `: ${props.alt}` : ""}</span>;
  return <img {...props} loading="lazy" decoding="async" referrerPolicy="no-referrer" onError={() => setFailed(true)} />;
}
function MarkdownTable({ node: _node, ref: _ref, ...props }: ComponentProps<"table"> & { node?: unknown }) {
  const table = useRef<HTMLTableElement>(null);
  const [status, setStatus] = useState("");
  useEffect(() => {
    if (!status) return;
    const timer = setTimeout(() => setStatus(""), 2500);
    return () => clearTimeout(timer);
  }, [status]);
  return <div data-streamdown="table-wrapper">
    <div className="agent-rich-table-toolbar">
      <span>Table</span><span role="status">{status}</span>
      <button type="button" onClick={async () => {
        if (!table.current) return;
        try { await navigator.clipboard.writeText(tableDataToTSV(extractTableDataFromElement(table.current))); setStatus("Copied"); }
        catch { setStatus("Couldn’t copy. Select cells to copy."); }
      }}>Copy table</button>
      <button type="button" onClick={() => {
        if (!table.current) return;
        const url = URL.createObjectURL(new Blob([tableDataToCSV(extractTableDataFromElement(table.current))], { type: "text/csv;charset=utf-8" }));
        const link = document.createElement("a"); link.href = url; link.download = "table.csv"; link.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
      }}>Save CSV</button>
    </div>
    <div className="agent-rich-table-scroll" role="region" aria-label="Table, scroll horizontally for more columns" tabIndex={0}>
      <table {...props} ref={table} />
    </div>
  </div>;
}
const components = { a: MarkdownLink, input: MarkdownInput, img: MarkdownImage, table: MarkdownTable };

/** Shared, sanitized rich content for responses and generated tool output.
 * Raw HTML never enters this document; closed ```html fences become sandboxed
 * previews whose props stay stable while later tokens stream in. */
export const RichMarkdown = memo(function RichMarkdown({ children, streaming = false, htmlPreviews = true }: {
  children: string;
  streaming?: boolean;
  /** Render closed ```html fences as previews; false keeps them as code. */
  htmlPreviews?: boolean;
}) {
  const parts = useMemo(() => htmlPreviews ? splitHtmlFences(children) : undefined, [children, htmlPreviews]);
  if (!parts) return <MarkdownBlock text={children} streaming={streaming} />;
  if (parts.length === 1 && parts[0]!.kind === "markdown") return <MarkdownBlock text={children} streaming={streaming} />;
  return <div className="agent-rich-parts">
    {parts.map((part, index) => part.kind === "html"
      ? <HtmlPreview key={`html:${part.offset}`} html={part.text} />
      : <MarkdownBlock key={`md:${part.offset}`} text={part.text} streaming={streaming && index === parts.length - 1} />)}
  </div>;
});

const MarkdownBlock = memo(function MarkdownBlock({ text, streaming }: { text: string; streaming: boolean }) {
  return <Streamdown className="agent-rich-markdown" components={components} plugins={plugins}
    controls={controls} mermaid={diagramOptions} linkSafety={linkSafety} skipHtml
    mode={streaming ? "streaming" : "static"} isAnimating={streaming}
    caret={streaming ? "block" : undefined}>{text}</Streamdown>;
});
