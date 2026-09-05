export interface PageCandidate {
  selector: string;
  tag: string;
  role?: string;
  text?: string;
  bounds: { x: number; y: number; width: number; height: number };
}

export interface PageSnapshot {
  document_revision: string;
  url: string;
  title: string;
  viewport: { width: number; height: number };
  candidates: PageCandidate[];
  truncated: boolean;
}

export const PREVIEW_STYLE_ID = "nanocodex-preview-v1";
export const PERSISTED_STYLE_ID = "nanocodex-persisted-v1";

/** Runs in Chrome's isolated scripting world. Keep this function self-contained. */
export function inspectPage(): PageSnapshot {
  const MAX_CANDIDATES = 500;
  const MAX_TOTAL_TEXT = 60_000;
  const candidates: PageCandidate[] = [];
  let totalText = 0;
  let truncated = false;
  const selectorFor = (element: Element): string => {
    if (element.id && /^[A-Za-z][\w:.-]{0,127}$/.test(element.id)) {
      return `#${CSS.escape(element.id)}`;
    }
    for (const attribute of ["data-testid", "data-test", "aria-label", "name"]) {
      const value = element.getAttribute(attribute);
      if (value && value.length <= 120) {
        return `${element.localName}[${attribute}="${CSS.escape(value)}"]`;
      }
    }
    const parts: string[] = [];
    let current: Element | null = element;
    while (current && current !== document.documentElement && parts.length < 5) {
      let part = current.localName;
      const parent: Element | null = current.parentElement;
      if (parent) {
        const siblings = Array.from(parent.children).filter((child: Element) => child.localName === current!.localName);
        if (siblings.length > 1) part += `:nth-of-type(${siblings.indexOf(current) + 1})`;
      }
      parts.unshift(part);
      current = parent;
    }
    return parts.join(" > ");
  };
  const elements = document.querySelectorAll(
    "main,nav,aside,header,footer,section,article,h1,h2,h3,button,a,input,select,textarea,[role],[aria-label],[data-testid]",
  );
  for (const element of elements) {
    if (candidates.length >= MAX_CANDIDATES || totalText >= MAX_TOTAL_TEXT) {
      truncated = true;
      break;
    }
    const bounds = element.getBoundingClientRect();
    const style = getComputedStyle(element);
    if (style.display === "none" || style.visibility === "hidden" || bounds.width <= 0 || bounds.height <= 0) continue;
    const rawText = element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement
      ? ""
      : (element.textContent ?? "").replace(/\s+/g, " ").trim();
    const text = rawText.slice(0, Math.min(240, MAX_TOTAL_TEXT - totalText));
    totalText += text.length;
    const role = element.getAttribute("role") ?? undefined;
    candidates.push({
      selector: selectorFor(element),
      tag: element.localName,
      ...(role ? { role } : {}),
      ...(text ? { text } : {}),
      bounds: {
        x: Math.round(bounds.x),
        y: Math.round(bounds.y),
        width: Math.round(bounds.width),
        height: Math.round(bounds.height),
      },
    });
  }
  const safeUrl = new URL(location.href);
  safeUrl.search = "";
  safeUrl.hash = "";
  const root = document.documentElement;
  const documentRevision = root.dataset.nanocodexDocumentRevision ?? crypto.randomUUID();
  root.dataset.nanocodexDocumentRevision = documentRevision;
  return {
    document_revision: documentRevision,
    url: safeUrl.href,
    title: document.title.slice(0, 300),
    viewport: { width: innerWidth, height: innerHeight },
    candidates,
    truncated,
  };
}

/** Runs in Chrome's isolated scripting world. */
export function installPreview(css: string): void {
  document.getElementById("nanocodex-preview-v1")?.remove();
  const style = document.createElement("style");
  style.id = "nanocodex-preview-v1";
  style.dataset.nanocodex = "preview";
  style.textContent = css;
  (document.head ?? document.documentElement).append(style);
}

/** Runs in Chrome's isolated scripting world. */
export function removePreview(): boolean {
  const style = document.getElementById("nanocodex-preview-v1");
  if (!style) return false;
  style.remove();
  return true;
}

/** Runs in Chrome's isolated scripting world. */
export function commitPreview(css: string): void {
  document.getElementById("nanocodex-preview-v1")?.remove();
  document.getElementById("nanocodex-persisted-v1")?.remove();
  const style = document.createElement("style");
  style.id = "nanocodex-persisted-v1";
  style.dataset.nanocodex = "persisted";
  style.textContent = css;
  (document.head ?? document.documentElement).append(style);
}

/** Runs in Chrome's isolated scripting world. */
export function removePersistedRecipe(): boolean {
  const style = document.getElementById("nanocodex-persisted-v1");
  if (!style) return false;
  style.remove();
  return true;
}
