import {
  getFiletypeFromFileName,
  type SupportedLanguages,
} from "@pierre/diffs";

const basenameOverrides: Record<string, SupportedLanguages> = {
  "Cargo.lock": "toml",
  Justfile: "just",
  "uv.lock": "toml",
};

export function syntaxLanguageForFile(path: string, contents?: string): SupportedLanguages {
  const basename = path.split("/").at(-1) ?? path;
  const override = basenameOverrides[basename];
  if (override) return override;
  if (/dockerfile/i.test(basename)) return "dockerfile";
  if (/^\.env(?:\.|$)/.test(basename)) return "dotenv";

  const inferred = getFiletypeFromFileName(basename);
  if (inferred !== "text") return inferred;

  const shebang = contents?.split("\n", 1)[0] ?? "";
  if (/^#!.*\b(?:ba|z|k)?sh\b/.test(shebang)) return "sh";
  if (/^#!.*\bpython(?:\d+(?:\.\d+)*)?\b/.test(shebang)) return "python";
  return "text";
}
