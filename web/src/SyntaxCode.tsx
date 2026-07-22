import { memo, useEffect, useState } from "react";
import type { BundledLanguage, ThemeRegistrationRaw } from "shiki";

const MAX_HIGHLIGHT_CHARS = 50_000;
const MAX_CACHE_ENTRIES = 256;
const htmlCache = new Map<string, Promise<string>>();

const languageAliases: Record<string, BundledLanguage> = {
  bash: "bash",
  c: "c",
  cpp: "cpp",
  css: "css",
  diff: "diff",
  dockerfile: "dockerfile",
  go: "go",
  html: "html",
  javascript: "javascript",
  js: "javascript",
  json: "json",
  jsx: "jsx",
  markdown: "markdown",
  md: "markdown",
  python: "python",
  py: "python",
  rs: "rust",
  rust: "rust",
  sh: "shellscript",
  shell: "shellscript",
  sql: "sql",
  toml: "toml",
  ts: "typescript",
  tsx: "tsx",
  typescript: "typescript",
  xml: "xml",
  yaml: "yaml",
  yml: "yaml",
  zig: "zig",
};

export const SyntaxCode = memo(function SyntaxCode({
  code,
  language,
  streaming = false,
  tree = false,
}: {
  code: string;
  language?: string;
  streaming?: boolean;
  tree?: boolean;
}) {
  const [html, setHtml] = useState<string>();
  const resolved = resolveLanguage(language);

  useEffect(() => {
    setHtml(undefined);
    // Re-highlighting every token swaps colored HTML for the plain fallback on
    // each streaming frame. Keep the live block stable, then color it once the
    // response seals.
    if (streaming || !code || code.length > MAX_HIGHLIGHT_CHARS) return;
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void highlightedHtml(code, resolved).then((value) => {
        if (!cancelled) setHtml(value);
      }).catch(() => {
        // Plain text remains visible when a grammar cannot be loaded.
      });
    }, 48);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [code, resolved, streaming]);

  const className = `tui-syntax${tree ? " is-tree" : ""}`;
  if (!html) {
    const lines = tree ? code.split("\n") : [];
    return tree ? (
      <pre className={`${className} is-plain`}><code>{lines.map((line, index) => (
        <span className="line" key={index}>{line || "\u00a0"}{index + 1 < lines.length ? "\n" : null}</span>
      ))}</code></pre>
    ) : <pre className={`${className} is-plain`}><code>{code}</code></pre>;
  }
  return <div className={className} dangerouslySetInnerHTML={{ __html: html }} />;
});

function resolveLanguage(language?: string): BundledLanguage {
  return languageAliases[(language ?? "").toLowerCase()] ?? "text";
}

function highlightedHtml(code: string, language: BundledLanguage): Promise<string> {
  const key = `${language}\0${code}`;
  const cached = htmlCache.get(key);
  if (cached) return cached;
  if (htmlCache.size >= MAX_CACHE_ENTRIES) {
    const oldest = htmlCache.keys().next().value;
    if (oldest !== undefined) htmlCache.delete(oldest);
  }
  const rendered = renderHighlightedHtml(code, language).catch((error) => {
    htmlCache.delete(key);
    throw error;
  });
  htmlCache.set(key, rendered);
  return rendered;
}

async function renderHighlightedHtml(code: string, language: BundledLanguage): Promise<string> {
  const [{ codeToHtml }, { default: light }, { default: dark }] = await Promise.all([
    import("shiki"),
    import("@pierre/theme/pierre-light"),
    import("@pierre/theme/pierre-dark"),
  ]);
  return codeToHtml(code, {
    lang: language,
    themes: {
      light: light as unknown as ThemeRegistrationRaw,
      dark: dark as unknown as ThemeRegistrationRaw,
    },
    defaultColor: false,
  });
}
