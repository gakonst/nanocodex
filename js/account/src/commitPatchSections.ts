import { getPatchMetadataHashes } from "./commitPatchMetadata.ts";
import { getStreamedPatchMetadata } from "./streamGitPatchFiles.ts";

export type StreamedCommitFileSections = {
  fileCommitHash: string | undefined;
  leadingHashes: string[];
  nextCommitHash: string | undefined;
  trailingHashes: string[];
};

export function inspectStreamedCommitFileSections(
  fileText: string,
  previousCommitHash?: string,
): StreamedCommitFileSections {
  const leadingMetadata = getStreamedPatchMetadata(fileText);
  const leadingHashes = leadingMetadata == null
    ? []
    : getPatchMetadataHashes(leadingMetadata);
  const fileCommitHash = leadingHashes.at(-1) ?? previousCommitHash;
  const trailingText = leadingMetadata == null
    ? fileText
    : fileText.slice(leadingMetadata.length);
  const trailingHashes = getPatchMetadataHashes(trailingText);
  return {
    fileCommitHash,
    leadingHashes,
    nextCommitHash: trailingHashes.at(-1) ?? fileCommitHash,
    trailingHashes,
  };
}
