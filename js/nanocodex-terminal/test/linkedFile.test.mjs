import assert from "node:assert/strict";
import test from "node:test";
import { renderToStaticMarkup } from "react-dom/server";
import { createElement } from "react";
import { AgentFileProvider, logicalFilePath } from "../dist/index.js";
import { RichMarkdown } from "../dist/RichMarkdown.js";

test("logical file links are recognized and web links are not", () => {
  assert.equal(logicalFilePath("/brain/outputs/frontiers-deck/frontiers.html"), "/brain/outputs/frontiers-deck/frontiers.html");
  assert.equal(logicalFilePath("file:///brain/My%20Film.mp4"), "/brain/My Film.mp4");
  assert.equal(logicalFilePath("/paradigm/src/main.rs:12"), "/paradigm/src/main.rs");
  assert.equal(logicalFilePath("https://example.com/a.mp4"), undefined);
  assert.equal(logicalFilePath("//evil.example/a.mp4"), undefined);
  assert.equal(logicalFilePath("/brain/../etc/passwd"), undefined);
});

test("linked decks and videos get inline preview slots when a reader exists", () => {
  const markdown = "[Deck](/brain/outputs/deck.html) and [film](/brain/outputs/videos/up.mp4) and [site](https://x.com)";
  const read = async () => new Blob([]);
  const html = renderToStaticMarkup(createElement(AgentFileProvider, { read }, createElement(RichMarkdown, null, markdown)));
  assert.match(html, /agent-linked-file is-html/);
  assert.match(html, /agent-linked-file is-video/);
  const plain = renderToStaticMarkup(createElement(RichMarkdown, null, markdown));
  assert.doesNotMatch(plain, /agent-linked-file/);
});
