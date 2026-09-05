export default defineContentScript({
  registration: "runtime",
  runAt: "document_start",
  async main() {
    const response = await chrome.runtime.sendMessage({ type: "recipe.for_document", url: location.href }) as
      | { css: string }
      | undefined;
    if (!response?.css) return;
    const style = document.createElement("style");
    style.id = "nanocodex-persisted-v1";
    style.dataset.nanocodex = "persisted";
    style.textContent = response.css;
    (document.head ?? document.documentElement).append(style);
  },
});
