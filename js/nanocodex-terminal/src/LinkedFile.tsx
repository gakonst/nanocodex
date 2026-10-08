import { createContext, memo, useContext, useEffect, useRef, useState, type ComponentProps, type MouseEvent, type ReactNode } from "react";
import { HtmlPreview } from "./HtmlPreview.js";

/** Reads one logical agent file (for example `/brain/outputs/a.mp4`). */
export type AgentFileReader = (path: string, signal: AbortSignal) => Promise<Blob>;

const AgentFileContext = createContext<AgentFileReader | undefined>(undefined);

/** Enables inline previews of logical file links (`/brain/...`, `/hand/...`). */
export function AgentFileProvider({ read, children }: { read: AgentFileReader | undefined; children: ReactNode }) {
  return <AgentFileContext.Provider value={read}>{children}</AgentFileContext.Provider>;
}

type Kind = "video" | "audio" | "image" | "html" | "pdf";
const KINDS: Record<string, [Kind, string]> = {
  mp4: ["video", "video/mp4"], m4v: ["video", "video/mp4"], mov: ["video", "video/quicktime"], webm: ["video", "video/webm"],
  mp3: ["audio", "audio/mpeg"], wav: ["audio", "audio/wav"], m4a: ["audio", "audio/mp4"], ogg: ["audio", "audio/ogg"],
  png: ["image", "image/png"], jpg: ["image", "image/jpeg"], jpeg: ["image", "image/jpeg"], gif: ["image", "image/gif"],
  webp: ["image", "image/webp"], html: ["html", "text/html"], htm: ["html", "text/html"], pdf: ["pdf", "application/pdf"],
};

/** Canonical logical path for a markdown href, or undefined for web links. */
export function logicalFilePath(href: string | undefined): string | undefined {
  if (!href) return undefined;
  let path = href;
  if (/^file:\/\//i.test(path)) path = path.replace(/^file:\/\/(localhost)?/i, "");
  else if (!path.startsWith("/") || path.startsWith("//")) return undefined;
  path = path.split(/[?#]/, 1)[0]!.replace(/:\d+(:\d+)?$/, "");
  try { path = decodeURIComponent(path); } catch { return undefined; }
  const segments = path.split("/").slice(1);
  if (!segments.length || segments.some(s => !s || s === "." || s === "..") || /[\u0000-\u001f\\]/.test(path)) return undefined;
  return path;
}

export function linkedFileKind(path: string): [Kind, string] | undefined {
  const extension = path.slice(path.lastIndexOf(".") + 1).toLowerCase();
  return path.includes(".") ? KINDS[extension] : undefined;
}

/** Markdown anchor: logical files open through the agent file reader and
 * playable media, images, PDFs and HTML decks preview inline. */
export function MarkdownLink({ node: _node, ref: _ref, href, children, ...props }: ComponentProps<"a"> & { node?: unknown }) {
  const read = useContext(AgentFileContext);
  const path = logicalFilePath(href);
  if (!read || !path) return <a href={href} {...props}>{children}</a>;
  return <LinkedFile path={path} read={read}>{children}</LinkedFile>;
}

const LinkedFile = memo(function LinkedFile({ path, read, children }: { path: string; read: AgentFileReader; children: ReactNode }) {
  const kind = linkedFileKind(path);
  const name = path.slice(path.lastIndexOf("/") + 1);
  const holder = useRef<HTMLSpanElement>(null);
  const [near, setNear] = useState(false);
  const [state, setState] = useState<{ url?: string; html?: string; error?: string }>({});

  useEffect(() => {
    if (!kind) return;
    const element = holder.current;
    if (!element || typeof IntersectionObserver === "undefined") { setNear(true); return; }
    const observer = new IntersectionObserver(entries => {
      if (entries.some(entry => entry.isIntersecting)) { setNear(true); observer.disconnect(); }
    }, { rootMargin: "600px 0px" });
    observer.observe(element);
    return () => observer.disconnect();
  }, [kind]);

  useEffect(() => {
    if (!kind || !near) return;
    const controller = new AbortController();
    let url: string | undefined;
    setState({});
    read(path, controller.signal).then(async blob => {
      if (kind[0] === "html") { setState({ html: await blob.text() }); return; }
      url = URL.createObjectURL(new Blob([blob], { type: kind[1] }));
      setState({ url });
    }).catch(error => {
      if (!controller.signal.aborted) setState({ error: error instanceof Error ? error.message : "File unavailable" });
    });
    return () => { controller.abort(); if (url) URL.revokeObjectURL(url); };
  }, [kind, near, path, read]);

  const open = async (event: MouseEvent<HTMLAnchorElement>) => {
    event.preventDefault();
    try {
      const blob = await read(path, new AbortController().signal);
      const url = URL.createObjectURL(kind ? new Blob([blob], { type: kind[1] }) : blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      if (kind && kind[0] !== "html") anchor.target = "_blank"; else anchor.download = name;
      anchor.rel = "noopener noreferrer";
      anchor.click();
      setTimeout(() => URL.revokeObjectURL(url), 60_000);
    } catch (error) {
      setState(current => ({ ...current, error: error instanceof Error ? error.message : "File unavailable" }));
    }
  };

  const link = <a href={`#${encodeURIComponent(path)}`} title={path} onClick={open}>{children}</a>;
  if (!kind) return link;
  let preview: ReactNode = <span className="agent-linked-file-status">Loading {name}…</span>;
  if (state.error) preview = <span className="agent-linked-file-status">{name} — {state.error}</span>;
  else if (state.html !== undefined) preview = <HtmlPreview html={state.html} name={name} />;
  else if (state.url) {
    const [type] = kind;
    preview = type === "video" ? <video src={state.url} controls playsInline preload="metadata" aria-label={name} />
      : type === "audio" ? <audio src={state.url} controls aria-label={name} />
      : type === "image" ? <img src={state.url} alt={name} loading="lazy" decoding="async" />
      : <iframe src={state.url} title={name} sandbox="" />;
  }
  return <>{link}<span ref={holder} className={`agent-linked-file is-${kind[0]}`} data-path={path}>{preview}</span></>;
});
