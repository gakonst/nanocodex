import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const css = readFileSync(new URL("../src/Docs.css", import.meta.url), "utf8");
const source = readFileSync(new URL("../src/Docs.tsx", import.meta.url), "utf8");
const modal = readFileSync(new URL("../src/modalBoundary.ts", import.meta.url), "utf8");
const syntax = readFileSync(new URL("../src/docsSyntax.tsx", import.meta.url), "utf8");

test("documentation uses a full-width shell and restrained heading scale", () => {
  assert.match(css, /\.docs-layout \{[\s\S]*?width:\s*100%;[\s\S]*?padding:\s*40px var\(--page-margin\) 96px/);
  assert.match(css, /\.docs-article h1 \{[\s\S]*?font-size:\s*22px/);
  assert.match(css, /\.docs-article h2 \{[\s\S]*?font-size:\s*16px/);
});

test("documentation figures render responsive diagrams with accessible text", () => {
  assert.match(source, /<figure className="docs-figure">/);
  assert.match(source, /<img src=\{block\.src\} alt=\{block\.alt\} loading="lazy" decoding="async"/);
  assert.match(source, /block\.caption \? <figcaption>/);
  assert.match(css, /\.docs-figure \{[\s\S]*?width:\s*min\(980px,/);
  assert.match(css, /\.docs-figure img \{[\s\S]*?width:\s*100%;[\s\S]*?height:\s*auto/);
  assert.match(css, /@media \(max-width: 900px\) \{[\s\S]*?\.docs-figure \{ width:\s*calc\(100vw - 24px\)/);
});

test("documentation navigation stays client-side and code uses themed syntax tokens", () => {
  assert.match(source, /import \{ Link, useLocation, useNavigate \} from "react-router"/);
  assert.match(source, /\{previous \? \([\s\S]*?<Link[\s\S]*?to=\{previous\.href\}/);
  assert.match(source, /to=\{page\.href\}/);
  assert.doesNotMatch(source, /<a href="\/docs"/);
  assert.match(source, /highlightDocsCode\(code, language\)/);
  assert.match(syntax, /createHighlighterCore\(\{/);
  assert.doesNotMatch(syntax, /createHighlighterCoreSync/);
  for (const language of ["bash", "javascript", "python", "rust", "tsx"]) {
    assert.match(syntax, new RegExp(`from "@shikijs/langs/${language}"`));
  }
  assert.match(syntax, /pierre-light/);
  assert.match(syntax, /pierre-dark-soft/);
  assert.match(css, /--shiki-light/);
  assert.match(css, /--shiki-dark/);
});

test("documentation drawers and pagination keep mobile targets and focus containment", () => {
  assert.match(css, /\.docs-drawer nav a \{[\s\S]*?min-height:\s*44px/);
  assert.match(css, /\.docs-pagination > a \{[\s\S]*?min-height:\s*44px/);
  assert.match(css, /@media \(pointer: coarse\), \(any-pointer: coarse\) \{[\s\S]*?\.docs-sidebar a,[\s\S]*?min-height:\s*44px/);
  assert.match(source, /useModalBoundary\(\{/);
  assert.match(source, /fallbackFocusRef: desktopFocusRef/);
  assert.match(modal, /containModalFocus/);
  assert.match(modal, /wrappedModalFocusIndex/);
  assert.match(modal, /document\.addEventListener\("focusin", onFocusIn, true\)/);
  assert.match(source, /matchMedia\("\(min-width: 901px\)"\)/);
});

test("documentation loads one Markdown source at a time and swaps only complete pages", () => {
  const navigation = readFileSync(new URL("../src/docsNavigation.ts", import.meta.url), "utf8");
  assert.match(navigation, /import\.meta\.glob\("\.\.\/docs\/src\/pages\/\*\*\/\*\.mdx"/);
  assert.doesNotMatch(navigation, /eager:\s*true/);
  assert.match(navigation, /sourceCache = new Map/);
  assert.match(source, /if \(!resolved\) return null/);
  assert.match(source, /if \(!resolved \|\| resolved\.path !== path\) return/);
  assert.match(source, /const doc = parseDocument\(source\)/);
  assert.match(source, /import \* as docsSyntax from "\.\/docsSyntax"/);
  assert.match(source, /await docsSyntax\.prepareDocsLanguages\(codeBlocks\.map\(\(block\) => block\.language\)\)/);
  assert.match(source, /for \(const block of codeBlocks\) docsSyntax\.highlightDocsCode\(block\.code, block\.language\)/);
  assert.match(source, /resolvedPageRequests\.get\(path\)[\s\S]*?resolvedPageRequests\.set\(path, request\)/);
  assert.match(source, /resolvedPageCache\.set\(path, \{[\s\S]*?doc,/);
  assert.match(source, /if \(next\) setResolved\(next\)/);
});

test("documentation navigation is flat and active pages are bold without decoration", () => {
  const navigation = readFileSync(new URL("../src/docsNavigation.ts", import.meta.url), "utf8");
  assert.match(navigation, /label: "JavaScript \/ TypeScript"/);
  assert.match(source, /className="docs-nav-section-heading"/);
  assert.doesNotMatch(source, /function DocsNavSection/);
  assert.doesNotMatch(source, /className="docs-nav-section-state"/);
  assert.doesNotMatch(source, /aria-controls=\{controls\}/);
  assert.doesNotMatch(source, /aria-expanded=\{expanded\}/);
  assert.doesNotMatch(source, /docs-nav-children/);
  assert.match(css, /a\[aria-current="page"\][\s\S]*?font-weight:\s*700/);
  assert.match(css, /\.docs-sidebar a,[\s\S]*?text-decoration:\s*none/);
  assert.doesNotMatch(css, /aria-current="page"\]\s*::before/);
  assert.doesNotMatch(css, /aria-current="page"\][^{]*\{[^}]*background:/);
  assert.match(css, /\.docs-drawer \{[\s\S]*?border-right:\s*1px solid var\(--border-soft\)/);
  assert.doesNotMatch(css, /\.docs-drawer \{[\s\S]*?box-shadow:/);
});

test("desktop documentation navigation is sticky and independently scrollable", () => {
  assert.match(css, /\.docs-sidebar,[\s\S]*?position:\s*sticky;[\s\S]*?max-height:\s*calc\(100vh - 56px\);[\s\S]*?overflow-y:\s*auto/);
  assert.match(css, /\.docs-sidebar \{[\s\S]*?scrollbar-width:\s*none/);
  assert.match(css, /\.docs-sidebar::\-webkit-scrollbar \{[\s\S]*?display:\s*none/);
});
