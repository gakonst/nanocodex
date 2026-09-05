export const COMMIT_HASH_METADATA_PATTERN = /^From\s+([a-f0-9]+)\s/im;
const COMMIT_HASH_METADATA_LINES_PATTERN = /^From\s+([a-f0-9]{40})\s.*$/gim;

const commitPrefixEncoder = new TextEncoder();
const commitPrefixDecoder = new TextDecoder();

export function getPatchTreePathPrefix(
  patchMetadata: string | undefined,
  patchIndex: number,
): string {
  const commitHash = patchMetadata?.match(COMMIT_HASH_METADATA_PATTERN)?.[1];
  return commitHash != null
    ? detachCommitPrefix(commitHash.slice(0, 5))
    : `Commit ${patchIndex + 1}`;
}

export function getPatchMetadataHashes(patchText: string): string[] {
  return [...patchText.matchAll(COMMIT_HASH_METADATA_LINES_PATTERN)]
    .map((match) => match[1]);
}

function detachCommitPrefix(value: string): string {
  return commitPrefixDecoder.decode(commitPrefixEncoder.encode(value));
}
