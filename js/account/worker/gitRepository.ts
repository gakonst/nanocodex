const SHA1_PATTERN = /^[a-f0-9]{40}$/;
const REF_PATTERN = /^refs\/(heads|tags)\/[A-Za-z0-9][A-Za-z0-9._\/-]*$/;
export const MAX_REPOSITORY_PART_BYTES = 16 * 1024 * 1024;
const MIN_REPOSITORY_MULTIPART_BYTES = 1024 * 1024;

export type RepositoryRef = {
  name: string;
  oid: string;
  peeled?: string;
};

export type RepositoryPart = {
  key: string;
  size: number;
};

export type RepositoryPartsManifest = {
  version: 1;
  head: string;
  parts: RepositoryPart[];
  size: number;
};

export type RepositoryPublication = {
  version: 1;
  head: string;
  branch: string;
  refs: RepositoryRef[];
  snapshotKey: string;
  commitsKey: string;
  commitPatchParts: RepositoryPart[];
  commitPatchSize: number;
  inventoryKey: string;
  packParts: RepositoryPart[];
  packSize: number;
  objectManifestKey: string;
  packHash: string;
  publishedAt: string;
};

type PublishRequest = {
  expectedHead: string | null;
  publication: RepositoryPublication;
  replaceInvalid?: true;
};

const publicationStorageKey = "publication";

export class GitRepository {
  readonly #state: DurableObjectState;

  constructor(state: DurableObjectState) {
    this.#state = state;
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname !== "/publication") {
      return Response.json({ error: "not_found" }, { status: 404 });
    }
    if (request.method === "GET") {
      const publication = await this.#state.storage.get<RepositoryPublication>(
        publicationStorageKey,
      );
      return publication == null
        ? Response.json({ error: "not_published" }, { status: 404 })
        : Response.json(publication);
    }
    if (request.method !== "PUT") {
      return new Response(null, { status: 405 });
    }

    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return Response.json({ error: "invalid_json" }, { status: 400 });
    }
    if (!isPublishRequest(body)) {
      return Response.json({ error: "invalid_publication" }, { status: 400 });
    }

    return this.#state.blockConcurrencyWhile(async () => {
      const current = await this.#state.storage.get<unknown>(
        publicationStorageKey,
      );
      const validCurrent = isRepositoryPublication(current) ? current : undefined;
      if (body.replaceInvalid === true) {
        if (current == null || validCurrent != null) {
          return Response.json(
            {
              error: "publication_repair_conflict",
              currentHead: validCurrent?.head ?? null,
            },
            { status: 409 },
          );
        }
      } else if (validCurrent == null && current != null) {
        return Response.json({ error: "publication_invalid" }, { status: 409 });
      } else if ((validCurrent?.head ?? null) !== body.expectedHead) {
        return Response.json(
          { error: "publication_conflict", currentHead: validCurrent?.head ?? null },
          { status: 409 },
        );
      }
      await this.#state.storage.put(publicationStorageKey, body.publication);
      return Response.json(body.publication);
    });
  }
}

export function isRepositoryPublication(
  value: unknown,
): value is RepositoryPublication {
  if (value == null || typeof value !== "object") return false;
  const publication = value as Partial<RepositoryPublication>;
  if (
    publication.version !== 1 ||
    typeof publication.head !== "string" ||
    !SHA1_PATTERN.test(publication.head) ||
    typeof publication.branch !== "string" ||
    !/^[A-Za-z0-9][A-Za-z0-9._\/-]*$/.test(publication.branch) ||
    !Array.isArray(publication.refs) ||
    typeof publication.publishedAt !== "string" ||
    !Number.isFinite(Date.parse(publication.publishedAt)) ||
    typeof publication.packHash !== "string" ||
    !SHA1_PATTERN.test(publication.packHash)
  ) {
    return false;
  }
  if (
    !publication.refs.every(
      (ref) =>
        ref != null &&
        typeof ref === "object" &&
        typeof ref.name === "string" &&
        REF_PATTERN.test(ref.name) &&
        typeof ref.oid === "string" &&
        SHA1_PATTERN.test(ref.oid) &&
        (ref.peeled === undefined ||
          (typeof ref.peeled === "string" && SHA1_PATTERN.test(ref.peeled))),
    )
  ) {
    return false;
  }
  const prefix = `generations/${publication.head}/`;
  if (
    !areCanonicalPatchPages(
      publication.commitPatchParts,
      publication.commitPatchSize,
      (index) => `${prefix}commit-patches/${String(index).padStart(4, "0")}.diff`,
    ) ||
    !areCanonicalByteParts(
      publication.packParts,
      publication.packSize,
      (index) =>
        `${prefix}packs/${publication.packHash}/${String(index).padStart(4, "0")}.pack`,
    )
  ) {
    return false;
  }
  return publication.snapshotKey === `${prefix}repository.json` &&
    publication.commitsKey === `${prefix}commits.json` &&
    publication.inventoryKey === `${prefix}inventory.json` &&
    publication.objectManifestKey === `${prefix}objects.json`;
}

export function isCommitPatchManifest(
  value: unknown,
  expectedHead: string,
): value is RepositoryPartsManifest {
  if (value == null || typeof value !== "object") return false;
  const manifest = value as Partial<RepositoryPartsManifest>;
  return manifest.version === 1 &&
    manifest.head === expectedHead &&
    areCanonicalPatchPages(
      manifest.parts,
      manifest.size,
      (index) =>
        `generations/${expectedHead}/commit-patches/${String(index).padStart(4, "0")}.diff`,
    );
}

function areCanonicalPatchPages(
  value: unknown,
  totalSize: unknown,
  keyAt: (index: number) => string,
): value is RepositoryPart[] {
  return areCanonicalParts(value, totalSize, keyAt, false);
}

function areCanonicalByteParts(
  value: unknown,
  totalSize: unknown,
  keyAt: (index: number) => string,
): value is RepositoryPart[] {
  return areCanonicalParts(value, totalSize, keyAt, true);
}

function areCanonicalParts(
  value: unknown,
  totalSize: unknown,
  keyAt: (index: number) => string,
  requireCanonicalByteParts: boolean,
): value is RepositoryPart[] {
  if (
    !Array.isArray(value) ||
    value.length === 0 ||
    value.length > 256 ||
    !Number.isSafeInteger(totalSize) ||
    (totalSize as number) <= 0
  ) {
    return false;
  }
  let observedSize = 0;
  let canonicalBytePartSize: number | undefined;
  for (const [index, part] of value.entries()) {
    if (
      part == null ||
      typeof part !== "object" ||
      part.key !== keyAt(index) ||
      !Number.isSafeInteger(part.size) ||
      part.size <= 0 ||
      part.size > MAX_REPOSITORY_PART_BYTES
    ) {
      return false;
    }
    if (requireCanonicalByteParts) {
      const bytePartSize = canonicalBytePartSize ?? part.size;
      canonicalBytePartSize = bytePartSize;
      if (
        (value.length > 1 && bytePartSize < MIN_REPOSITORY_MULTIPART_BYTES) ||
        (index < value.length - 1 && part.size !== bytePartSize) ||
        (index === value.length - 1 && part.size > bytePartSize)
      ) {
        return false;
      }
    }
    observedSize += part.size;
  }
  return observedSize === totalSize;
}

function isPublishRequest(value: unknown): value is PublishRequest {
  if (value == null || typeof value !== "object") return false;
  const request = value as Partial<PublishRequest>;
  return (
    (request.expectedHead === null ||
      (typeof request.expectedHead === "string" &&
        SHA1_PATTERN.test(request.expectedHead))) &&
    (request.replaceInvalid === undefined || request.replaceInvalid === true) &&
    (request.replaceInvalid !== true || request.expectedHead === null) &&
    isRepositoryPublication(request.publication)
  );
}

export const gitObjectPatterns = {
  blob: SHA1_PATTERN,
  patch: SHA1_PATTERN,
  generation: SHA1_PATTERN,
} as const;
