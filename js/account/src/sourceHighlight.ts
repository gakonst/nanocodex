import {
  getFiletypeFromFileName,
  type CodeViewItem,
  type FileContents,
  type SupportedLanguages,
} from "@pierre/diffs";
import { syntaxLanguageForFile } from "./syntax";
import type { RepositoryFile } from "./threadRepositorySnapshot";

export function sourceFileContents(
  file: RepositoryFile,
  contents: string,
): FileContents {
  const lang = syntaxLanguageForFile(file.path, contents);
  return {
    name: file.path,
    contents,
    lang,
    // Blob identity alone is insufficient: the same bytes can be published at
    // two paths whose extensions select different grammars.
    cacheKey: `${file.objectId}:${lang}:${file.path}`,
  };
}

export function sourceCodeViewItem(
  file: RepositoryFile,
  contents: string,
): Extract<CodeViewItem<undefined>, { type: "file" }> {
  return {
    id: `file:${file.objectId}:${file.path}`,
    type: "file",
    file: sourceFileContents(file, contents),
  };
}

export function syntaxLanguagesForPaths(
  paths: readonly string[],
  maximum = 4,
): SupportedLanguages[] {
  const languages = new Set<SupportedLanguages>();
  for (const path of paths) {
    const language = syntaxLanguageForFile(path);
    if (language === "text" || language === "ansi") continue;
    languages.add(language);
    if (languages.size >= maximum) break;
  }
  return [...languages];
}

export function itemSyntaxLanguages(
  items: readonly CodeViewItem<undefined>[],
): SupportedLanguages[] {
  const languages = new Set<SupportedLanguages>();
  for (const item of items) {
    if (item.type === "file") {
      const language = item.file.lang ?? getFiletypeFromFileName(item.file.name);
      if (language !== "text" && language !== "ansi") languages.add(language);
      continue;
    }
    const language = item.fileDiff.lang
      ?? getFiletypeFromFileName(item.fileDiff.name);
    const previousLanguage = item.fileDiff.lang
      ?? getFiletypeFromFileName(item.fileDiff.prevName ?? "-");
    if (language !== "text" && language !== "ansi") languages.add(language);
    if (previousLanguage !== "text" && previousLanguage !== "ansi") {
      languages.add(previousLanguage);
    }
  }
  return [...languages];
}
