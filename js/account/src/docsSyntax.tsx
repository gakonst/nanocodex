import pierreDark from "@pierre/theme/pierre-dark-soft";
import pierreLight from "@pierre/theme/pierre-light";
import bash from "@shikijs/langs/bash";
import javascript from "@shikijs/langs/javascript";
import python from "@shikijs/langs/python";
import rust from "@shikijs/langs/rust";
import tsx from "@shikijs/langs/tsx";
import { Fragment, type CSSProperties, type ReactNode } from "react";
import { createHighlighterCore } from "shiki/core";
import { createJavaScriptRegexEngine } from "shiki/engine/javascript";
import type { LanguageInput } from "@shikijs/types";

type DocsLanguage = "bash" | "javascript" | "python" | "rust" | "tsx";

const languageAliases: Record<string, DocsLanguage | undefined> = {
  bash: "bash",
  js: "javascript",
  javascript: "javascript",
  py: "python",
  python: "python",
  rs: "rust",
  rust: "rust",
  sh: "bash",
  shell: "bash",
  shellscript: "bash",
  tsx: "tsx",
};
const languageInputs: Record<DocsLanguage, LanguageInput[]> = {
  bash,
  javascript,
  python,
  rust,
  tsx,
};

type DocsHighlighter = Awaited<ReturnType<typeof createHighlighterCore>>;
let highlighter: DocsHighlighter | undefined;
const highlighterRequest = createHighlighterCore({
  engine: createJavaScriptRegexEngine({ forgiving: true }),
  langs: [],
  themes: [toShikiTheme(pierreLight), toShikiTheme(pierreDark)],
}).then((value) => {
  highlighter = value;
  return value;
});
const loadedLanguages = new Set<DocsLanguage>();
let languageLoadSequence: Promise<void> = Promise.resolve();
const highlightCache = new Map<string, ReactNode>();
const MAX_HIGHLIGHT_CACHE_ENTRIES = 200;

function toShikiTheme(theme: typeof pierreLight) {
  return {
    bg: theme.colors["editor.background"],
    displayName: theme.displayName,
    fg: theme.colors["editor.foreground"],
    name: theme.name,
    settings: theme.tokenColors.map(({ name, scope, settings }) => ({
      name,
      scope,
      settings: { ...settings },
    })),
    type: theme.type,
  };
}

export function resolveDocsLanguage(language: string): DocsLanguage | undefined {
  return languageAliases[language.trim().toLowerCase()];
}

/** Loads only the grammars present in the complete page about to be committed. */
export function prepareDocsLanguages(languages: readonly string[]): Promise<void> {
  const required = [...new Set(
    languages
      .map(resolveDocsLanguage)
      .filter((language): language is DocsLanguage => language !== undefined),
  )];
  const request = languageLoadSequence.then(async () => {
    const missing = required.filter((language) => !loadedLanguages.has(language));
    if (missing.length === 0) return;
    const highlighter = await highlighterRequest;
    await highlighter.loadLanguage(...missing.flatMap((language) => languageInputs[language]));
    for (const language of missing) loadedLanguages.add(language);
  });
  languageLoadSequence = request.catch(() => undefined);
  return request;
}

export function highlightDocsCode(code: string, language: string): ReactNode {
  const resolved = resolveDocsLanguage(language);
  const preparedHighlighter = highlighter;
  if (!resolved || !preparedHighlighter || !loadedLanguages.has(resolved)) return code;
  const cacheKey = `${resolved}\0${code}`;
  const cached = highlightCache.get(cacheKey);
  if (cached !== undefined) return cached;

  try {
    const { tokens } = preparedHighlighter.codeToTokens(code, {
      defaultColor: false,
      lang: resolved,
      themes: {
        dark: "pierre-dark-soft",
        light: "pierre-light",
      },
    });
    const highlighted = tokens.map((line, lineIndex) => (
      <Fragment key={lineIndex}>
        {line.map((token) => (
          <span
            className="docs-code-token"
            key={token.offset}
            style={
              typeof token.htmlStyle === "object"
                ? token.htmlStyle as CSSProperties
                : undefined
            }
          >
            {token.content}
          </span>
        ))}
        {lineIndex < tokens.length - 1 ? "\n" : null}
      </Fragment>
    ));
    highlightCache.set(cacheKey, highlighted);
    if (highlightCache.size > MAX_HIGHLIGHT_CACHE_ENTRIES) {
      const oldest = highlightCache.keys().next().value;
      if (oldest !== undefined) highlightCache.delete(oldest);
    }
    return highlighted;
  } catch {
    return code;
  }
}
