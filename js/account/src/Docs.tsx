"use client";

import { Check, ChevronLeft, ChevronRight, Copy, Menu } from "lucide-react";
import {
  Fragment,
  createElement,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Link, useLocation, useNavigate } from "react-router";
import {
  parseDocument,
  type MarkdownBlock,
  type ParsedDoc,
} from "./docsMarkdown";
import {
  docsNavigation,
  docsPageOrder,
  hasDocsSource,
  loadDocsSource,
  normalizeDocsPath,
  type DocsPage,
} from "./docsNavigation";
import { useModalBoundary } from "./modalBoundary";
import * as docsSyntax from "./docsSyntax";
import "./Docs.css";

type ResolvedPage =
  | { kind: "document"; path: string; source: string; doc: ParsedDoc }
  | { kind: "missing"; path: string }
  | { kind: "error"; path: string };

const resolvedPageCache = new Map<string, ResolvedPage>();
const resolvedPageRequests = new Map<string, Promise<void>>();
export async function preloadDocsRoute(pathname: string) {
  const path = normalizeDocsPath(pathname);
  if (resolvedPageCache.has(path)) return;
  if (!hasDocsSource(path)) {
    resolvedPageCache.set(path, { kind: "missing", path });
    return;
  }
  const existing = resolvedPageRequests.get(path);
  if (existing) return existing;
  const request = resolveDocsPage(path);
  resolvedPageRequests.set(path, request);
  const release = () => {
    if (resolvedPageRequests.get(path) === request) resolvedPageRequests.delete(path);
  };
  void request.then(release, release);
  return request;
}

async function resolveDocsPage(path: string): Promise<void> {
  const source = await loadDocsSource(path);
  if (source == null) return;
  const doc = parseDocument(source);
  const codeBlocks = doc.blocks.filter(
    (block): block is Extract<MarkdownBlock, { type: "code" }> => block.type === "code",
  );
  await docsSyntax.prepareDocsLanguages(codeBlocks.map((block) => block.language));
  for (const block of codeBlocks) docsSyntax.highlightDocsCode(block.code, block.language);
  resolvedPageCache.set(path, { kind: "document", path, source, doc });
}

function highlightDocsCode(code: string, language: string): ReactNode {
  return docsSyntax.highlightDocsCode(code, language);
}

export function Docs() {
  const location = useLocation();
  const navigate = useNavigate();
  const path = normalizeDocsPath(location.pathname);
  const [resolved, setResolved] = useState<ResolvedPage | undefined>(() =>
    resolvedPageCache.get(path)
  );
  const [loadAttempt, setLoadAttempt] = useState(0);
  const [browseOpen, setBrowseOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const browseButtonRef = useRef<HTMLButtonElement>(null);
  const docsPageRef = useRef<HTMLDivElement>(null);
  const desktopFocusRef = useRef<HTMLDivElement>(null);
  const drawerBackdropRef = useRef<HTMLButtonElement>(null);
  const drawerRef = useRef<HTMLDivElement>(null);
  const drawerCloseRef = useRef<HTMLButtonElement>(null);
  const displayPath = resolved?.path ?? path;
  const currentIndex = docsPageOrder.findIndex(({ href }) => href === displayPath);
  const previous = currentIndex > 0 ? docsPageOrder[currentIndex - 1] : undefined;
  const next = currentIndex >= 0 ? docsPageOrder[currentIndex + 1] : undefined;
  const closeBrowse = useCallback(() => setBrowseOpen(false), []);

  useEffect(() => {
    let active = true;
    if (!hasDocsSource(path)) {
      setResolved({ kind: "missing", path });
      return () => {
        active = false;
      };
    }

    const cached = resolvedPageCache.get(path);
    if (cached) {
      setResolved(cached);
      return () => {
        active = false;
      };
    }

    void preloadDocsRoute(path).then(() => {
      if (!active) return;
      const next = resolvedPageCache.get(path);
      if (next) setResolved(next);
    }).catch(() => {
      if (active) setResolved({ kind: "error", path });
    });

    return () => {
      active = false;
    };
  }, [loadAttempt, path]);

  useEffect(() => {
    setBrowseOpen(false);
    setCopied(false);
  }, [path]);

  useEffect(() => {
    if (!resolved || resolved.path !== path) return;
    window.requestAnimationFrame(() => {
      const target = location.hash
        ? window.document.getElementById(decodeHash(location.hash))
        : null;
      if (target) target.scrollIntoView();
      else window.scrollTo({ top: 0 });
    });
    window.document.title = resolved.kind === "document"
      ? `${resolved.doc.title} · Nanocodex docs`
      : "Docs · Nanocodex";
  }, [location.hash, path, resolved]);

  useModalBoundary({
    backdropRef: drawerBackdropRef,
    fallbackFocusRef: desktopFocusRef,
    initialFocusRef: drawerCloseRef,
    onDismiss: closeBrowse,
    open: browseOpen,
    panelRef: drawerRef,
    returnFocusRef: browseButtonRef,
  });

  useEffect(() => {
    const desktop = window.matchMedia("(min-width: 901px)");
    const closeDrawerOnDesktop = (event: MediaQueryListEvent) => {
      if (event.matches) setBrowseOpen(false);
    };
    desktop.addEventListener("change", closeDrawerOnDesktop);
    return () => desktop.removeEventListener("change", closeDrawerOnDesktop);
  }, []);

  useEffect(() => {
    const pageWithKeyboardShortcuts = (event: KeyboardEvent) => {
      if (
        !event.shiftKey ||
        event.altKey ||
        event.ctrlKey ||
        event.metaKey ||
        event.defaultPrevented
      ) return;

      const destination = event.key === "ArrowLeft"
        ? previous
        : event.key === "ArrowRight"
          ? next
          : undefined;
      if (!destination || !isDocsPagingTarget(event.target, docsPageRef.current)) return;

      event.preventDefault();
      navigate(destination.href);
    };
    window.addEventListener("keydown", pageWithKeyboardShortcuts);
    return () => window.removeEventListener("keydown", pageWithKeyboardShortcuts);
  }, [navigate, next, previous]);

  if (!resolved) return null;

  if (resolved.kind === "missing") {
    return (
      <section className="docs-not-found">
        <p className="eyebrow">Nanocodex docs</p>
        <h1>That page is not in the manual.</h1>
        <Link to="/docs">Browse the documentation</Link>
      </section>
    );
  }

  if (resolved.kind === "error") {
    return (
      <section className="docs-not-found">
        <p className="eyebrow">Nanocodex docs</p>
        <h1>The documentation could not be loaded.</h1>
        <button type="button" onClick={() => setLoadAttempt((attempt) => attempt + 1)}>
          Try again
        </button>
      </section>
    );
  }

  const { doc, source } = resolved;
  const headings = doc.blocks.filter(
    (block): block is Extract<MarkdownBlock, { type: "heading" }> =>
      block.type === "heading" && block.depth === 2,
  );
  const copyMarkdown = () => {
    void navigator.clipboard.writeText(source).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1_500);
    });
  };

  return (
    <div className="docs-page" ref={docsPageRef}>
      <div className="docs-mobile-toolbar">
        <button
          ref={browseButtonRef}
          type="button"
          aria-expanded={browseOpen}
          aria-controls="docs-mobile-navigation"
          onClick={() => setBrowseOpen(true)}
        >
          <Menu aria-hidden="true" /> Browse
        </button>
        <CopyMarkdownButton copied={copied} onClick={copyMarkdown} />
      </div>

      <div className="docs-layout">
        <aside className="docs-sidebar">
          <DocsNavigation path={displayPath} />
        </aside>

        <div className="docs-reading-column" ref={desktopFocusRef} tabIndex={-1}>
          <CopyMarkdownButton copied={copied} onClick={copyMarkdown} />
          <article className="docs-article" id="docs-content">
            {doc.blocks.map((block, index) => (
              <MarkdownBlockView block={block} key={`${block.type}-${index}`} />
            ))}
          </article>
          <nav className="docs-pagination" aria-label="Adjacent documentation pages">
            {previous ? (
              <Link
                to={previous.href}
                aria-keyshortcuts="Shift+ArrowLeft"
                title="Previous page (Shift + Left Arrow)"
              >
                <ChevronLeft aria-hidden="true" />
                <span><small>Previous</small>{previous.label}</span>
              </Link>
            ) : <span />}
            {next ? (
              <Link
                to={next.href}
                aria-keyshortcuts="Shift+ArrowRight"
                title="Next page (Shift + Right Arrow)"
              >
                <span><small>Next</small>{next.label}</span>
                <ChevronRight aria-hidden="true" />
              </Link>
            ) : null}
          </nav>
        </div>

        <aside className="docs-on-this-page" aria-label="On this page">
          <p>On this page</p>
          {headings.map((heading) => (
            <Link to={`#${heading.id}`} key={heading.id}>{stripMarkdown(heading.text)}</Link>
          ))}
        </aside>
      </div>

      {browseOpen ? (
        <div className="docs-drawer-layer">
          <button
            ref={drawerBackdropRef}
            className="docs-drawer-backdrop"
            type="button"
            aria-hidden="true"
            tabIndex={-1}
            onClick={closeBrowse}
          />
          <div
            className="docs-drawer"
            ref={drawerRef}
            id="docs-mobile-navigation"
            role="dialog"
            aria-modal="true"
            aria-label="Documentation navigation"
            tabIndex={-1}
          >
            <header>
              <span>Documentation</span>
              <button ref={drawerCloseRef} type="button" aria-label="Close" onClick={closeBrowse}>
                <span aria-hidden="true">×</span>
              </button>
            </header>
            <DocsNavigation path={displayPath} onNavigate={closeBrowse} />
          </div>
        </div>
      ) : null}
    </div>
  );
}

const docsPagingIgnoredTarget = [
  "input",
  "textarea",
  "select",
  "button",
  "iframe",
  '[contenteditable]:not([contenteditable="false"])',
  '[role="textbox"]',
  '[role="combobox"]',
  ".monaco-editor",
  ".cm-editor",
  ".CodeMirror",
  "[data-code-editor]",
].join(", ");

function isDocsPagingTarget(target: EventTarget | null, docsPage: HTMLElement | null) {
  if (!docsPage) return false;
  const element = target instanceof Element ? target : window.document.activeElement;
  if (!element) return false;
  if (element === window.document.body || element === window.document.documentElement) return true;
  return docsPage.contains(element) && !element.closest(docsPagingIgnoredTarget);
}

function DocsNavigation({ path, onNavigate }: { path: string; onNavigate?(): void }) {
  return (
    <nav aria-label="Documentation">
      {docsNavigation.map((group) => (
        <section className="docs-nav-group" key={group.label}>
          <p>{group.label}</p>
          <div className="docs-nav-items">
            {group.items.map((item) => item.type === "page" ? (
              <DocsNavLink key={item.href} page={item} path={path} onNavigate={onNavigate} />
            ) : (
              <Fragment key={item.id}>
                <p className="docs-nav-section-heading">{item.label}</p>
                {item.pages.map((page) => (
                  <DocsNavLink key={page.href} page={page} path={path} onNavigate={onNavigate} />
                ))}
              </Fragment>
            ))}
          </div>
        </section>
      ))}
    </nav>
  );
}

function DocsNavLink({
  onNavigate,
  page,
  path,
}: {
  onNavigate?(): void;
  page: DocsPage;
  path: string;
}) {
  return (
    <Link
      to={page.href}
      aria-current={page.href === path ? "page" : undefined}
      onClick={onNavigate}
    >
      {page.label}
    </Link>
  );
}

function CopyMarkdownButton({ copied, onClick }: { copied: boolean; onClick(): void }) {
  return (
    <button className="docs-copy-markdown" type="button" onClick={onClick}>
      {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
      <span>{copied ? "Copied" : "Copy markdown"}</span>
    </button>
  );
}

function MarkdownBlockView({ block }: { block: MarkdownBlock }) {
  if (block.type === "heading") {
    return createElement(
      `h${block.depth}`,
      { id: block.id },
      block.depth > 1 ? <Link to={`#${block.id}`}>{inline(block.text)}</Link> : inline(block.text),
    );
  }
  if (block.type === "paragraph") return <p>{inline(block.text)}</p>;
  if (block.type === "code") return <CodeBlock code={block.code} language={block.language} />;
  if (block.type === "list") {
    const List = block.ordered ? "ol" : "ul";
    return <List>{block.items.map((item, index) => <li key={index}>{inline(item)}</li>)}</List>;
  }
  return (
    <div className="docs-table-scroll">
      <table>
        <thead><tr>{block.headers.map((header) => <th key={header}>{inline(header)}</th>)}</tr></thead>
        <tbody>
          {block.rows.map((row, rowIndex) => (
            <tr key={rowIndex}>{row.map((cell, cellIndex) => <td key={cellIndex}>{inline(cell)}</td>)}</tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function CodeBlock({ code, language }: { code: string; language: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="docs-code-block">
      <span>{language || "text"}</span>
      <button
        type="button"
        aria-label="Copy code"
        onClick={() => {
          void navigator.clipboard.writeText(code).then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1_500);
          });
        }}
      >
        {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
      </button>
      <pre tabIndex={0}><code>{highlightDocsCode(code, language)}</code></pre>
    </div>
  );
}

function inline(value: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  const pattern = /(\[[^\]]+\]\([^)]+\)|`[^`]+`|\*\*[^*]+\*\*)/g;
  let cursor = 0;
  for (const match of value.matchAll(pattern)) {
    const start = match.index ?? 0;
    if (start > cursor) nodes.push(value.slice(cursor, start));
    const token = match[0];
    const link = token.match(/^\[([^\]]+)\]\(([^)]+)\)$/);
    if (link) {
      const href = docsHref(link[2]);
      nodes.push(isInternalHref(href) ? (
        <Link to={href} key={`${start}-${href}`}>{link[1]}</Link>
      ) : (
        <a
          href={href}
          target={/^https?:\/\//i.test(href) ? "_blank" : undefined}
          rel={/^https?:\/\//i.test(href) ? "noreferrer" : undefined}
          key={`${start}-${href}`}
        >
          {link[1]}
        </a>
      ));
    } else if (token.startsWith("`")) {
      nodes.push(<code key={start}>{token.slice(1, -1)}</code>);
    } else {
      nodes.push(<strong key={start}>{token.slice(2, -2)}</strong>);
    }
    cursor = start + token.length;
  }
  if (cursor < value.length) nodes.push(value.slice(cursor));
  return nodes.map((node, index) => <Fragment key={index}>{node}</Fragment>);
}

function decodeHash(hash: string) {
  try {
    return decodeURIComponent(hash.slice(1));
  } catch {
    return hash.slice(1);
  }
}

function docsHref(href: string) {
  if (
    href.startsWith("#") ||
    href.startsWith("/docs") ||
    isExternalHref(href) ||
    !href.startsWith("/")
  ) return href;
  return `/docs${href}`;
}

function isInternalHref(href: string) {
  return !isExternalHref(href) && (href.startsWith("/") || href.startsWith("#"));
}

function isExternalHref(href: string) {
  return /^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(href);
}

function stripMarkdown(value: string) {
  return value.replace(/\[([^\]]+)\]\([^)]+\)/g, "$1").replace(/[`*_]/g, "");
}
