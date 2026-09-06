import { utf8ByteLength } from "../runtime/utf8.mjs";

/** @deprecated Namespace size is governed by the selected providers. */
export const MAX_NAMESPACE_MOUNTS = Infinity;
export const MAX_NAMESPACE_GRANTS = 128;
export const MAX_NAMESPACE_PATH_BYTES = 4_096;
export const MAX_NAMESPACE_PATH_SEGMENTS = 256;
export const MAX_NAMESPACE_SEGMENT_BYTES = 255;
export const MAX_NAMESPACE_ID_BYTES = 256;

export const NAMESPACE_RIGHTS = Object.freeze([
  "namespace.discover",
  "filesystem.read",
  "filesystem.write",
  "process.exec",
  "process.stdin",
  "network.preview",
] as const);

export type NamespaceRight = (typeof NAMESPACE_RIGHTS)[number];

declare const namespaceIdentityBrand: unique symbol;
export type NamespaceIdentity = string & {
  readonly [namespaceIdentityBrand]: "NamespaceIdentity";
};

export type NamespaceMountInput = Readonly<{
  root: string;
  mountId: string;
  handId: string;
  exportId: string;
  generation: string;
  rights: readonly NamespaceRight[];
}>;

export type NamespaceManifestInput = Readonly<{
  manifestId: string;
  mounts: readonly NamespaceMountInput[];
}>;

export type NamespaceMount = Readonly<{
  root: string;
  mountId: NamespaceIdentity;
  handId: NamespaceIdentity;
  exportId: NamespaceIdentity;
  generation: NamespaceIdentity;
  rights: readonly NamespaceRight[];
}>;

export type NamespaceManifest = Readonly<{
  manifestId: NamespaceIdentity;
  mounts: readonly NamespaceMount[];
}>;

export type NamespaceGrant = Readonly<{
  mountId: NamespaceIdentity;
  path: string;
  rights: readonly NamespaceRight[];
}>;

export type NamespaceScope = Readonly<{
  manifest: NamespaceManifest;
  defaultCwd: string;
  grants: readonly NamespaceGrant[];
}>;

export type NamespaceAttenuation = Readonly<{
  mountId: string;
  path?: string;
  rights?: readonly NamespaceRight[];
}>;

export type NamespaceRoute = Readonly<{
  cwd: string;
  relativePath: string;
  mount: NamespaceMount;
  rights: readonly NamespaceRight[];
}>;

export class NamespaceError extends Error {
  constructor(
    readonly code:
      | "invalid_manifest"
      | "invalid_path"
      | "invalid_scope"
      | "access_denied"
      | "no_execution_root",
    message: string,
  ) {
    super(message);
    this.name = "NamespaceError";
  }
}

const PORTABLE_ROOT = /^[a-z0-9](?:[a-z0-9._-]{0,61}[a-z0-9])?$/;
const WINDOWS_DEVICE_ROOT = /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/;
const CONTROL_CHARACTER = /[\u0000-\u001f\u007f]/;
const HASH_UTF8 = new TextEncoder();
const RESERVED_ROOTS = new Set([
  ".nanocodex",
  "dev",
  "proc",
  "tmp",
]);
const RESERVED_HAND_ROOTS = new Set([
  ...RESERVED_ROOTS,
  "brain",
  "sandbox",
]);
const RIGHT_SET: ReadonlySet<string> = new Set(NAMESPACE_RIGHTS);
const admittedManifests = new WeakSet<object>();
const admittedScopes = new WeakSet<object>();

/**
 * Admits and snapshots a mount table. Calling this function is an authority
 * boundary: later APIs only preserve or attenuate mounts admitted here.
 */
export function createNamespaceManifest(input: NamespaceManifestInput): NamespaceManifest {
  if (!isRecord(input) || !Array.isArray(input.mounts)) {
    throw new NamespaceError("invalid_manifest", "namespace manifest must contain a mounts array");
  }
  if (input.mounts.length === 0) {
    throw new NamespaceError(
      "invalid_manifest",
      "namespace manifest must contain at least one mount",
    );
  }

  const manifestId = parseIdentity(input.manifestId, "manifestId");
  const roots = new Set<string>();
  const mountIds = new Set<string>();
  const mounts = input.mounts.map((candidate, index) => {
    if (!isRecord(candidate)) {
      throw new NamespaceError("invalid_manifest", `mount ${index} must be an object`);
    }
    const root = parseMountRoot(candidate.root, index);
    if (roots.has(root)) {
      throw new NamespaceError("invalid_manifest", `duplicate mount root ${root}`);
    }
    roots.add(root);

    const mountId = parseIdentity(candidate.mountId, `mount ${root} mountId`);
    if (mountIds.has(mountId)) {
      throw new NamespaceError("invalid_manifest", `duplicate mount identity for ${root}`);
    }
    mountIds.add(mountId);

    return Object.freeze({
      root,
      mountId,
      handId: parseIdentity(candidate.handId, `mount ${root} handId`),
      exportId: parseIdentity(candidate.exportId, `mount ${root} exportId`),
      generation: parseIdentity(candidate.generation, `mount ${root} generation`),
      rights: parseRights(candidate.rights, `mount ${root}`),
    });
  });

  // Canonical ordering makes the same admitted mapping stable regardless of
  // discovery order. Routing never consults display or host names.
  mounts.sort((left, right) => left.root.localeCompare(right.root, "en"));
  const manifest = Object.freeze({ manifestId, mounts: Object.freeze(mounts) });
  admittedManifests.add(manifest);
  return manifest;
}

/** Derives a deterministic portable mount root from an opaque machine identity. */
export function namespaceMountRoot(machineId: string): string {
  if (typeof machineId !== "string" || machineId.length === 0) {
    throw new TypeError("machine identity must be a non-empty string");
  }
  const exact = machineId.toLowerCase();
  if (exact === machineId
    && PORTABLE_ROOT.test(exact)
    && !WINDOWS_DEVICE_ROOT.test(exact)
    && !RESERVED_HAND_ROOTS.has(exact)) {
    return `/${exact}`;
  }
  const slug = exact.replace(/[^a-z0-9._-]+/g, "-").replace(/^[^a-z0-9]+|[^a-z0-9]+$/g, "")
    .slice(0, 48) || "hand";
  return `/hand-${slug}-${stableHash(machineId).slice(0, 8)}`;
}

/** Resolves an absolute logical path without consulting the host filesystem. */
export function normalizeNamespacePath(path: string): string {
  return normalizePath(path, undefined);
}

/** Resolves an optional workdir against a captured, absolute default cwd. */
export function resolveNamespaceCwd(defaultCwd: string, workdir?: string): string {
  const base = normalizeNamespacePath(defaultCwd);
  if (workdir === undefined || workdir === "") return base;
  return normalizePath(workdir, base);
}

/** Component-aware containment; lexical siblings such as /host2 never match /host. */
export function isNamespacePathWithin(path: string, parent: string): boolean {
  const normalizedPath = normalizeNamespacePath(path);
  const normalizedParent = normalizeNamespacePath(parent);
  return pathWithinNormalized(normalizedPath, normalizedParent);
}

/** Returns the longest component-boundary mount match, if one exists. */
export function resolveNamespaceMount(
  manifest: NamespaceManifest,
  path: string,
): NamespaceMount | undefined {
  assertManifest(manifest);
  const normalized = normalizeNamespacePath(path);
  let match: NamespaceMount | undefined;
  for (const mount of manifest.mounts) {
    if (pathWithinNormalized(normalized, mount.root)
      && (match === undefined || mount.root.length > match.root.length)) {
      match = mount;
    }
  }
  return match;
}

/** Creates the root scope with every right present in the admitted manifest. */
export function createNamespaceScope(
  manifest: NamespaceManifest,
  defaultCwd: string,
): NamespaceScope {
  assertManifest(manifest);
  const cwd = normalizeNamespacePath(defaultCwd);
  const grants = manifest.mounts.map((mount) => Object.freeze({
    mountId: mount.mountId,
    path: mount.root,
    rights: mount.rights,
  }));
  return admitScope(manifest, cwd, grants);
}

/**
 * Creates a child snapshot. Omitted attenuation inherits exactly; supplied
 * entries may only remove mounts, narrow paths, or remove rights.
 */
export function deriveChildNamespaceScope(
  parent: NamespaceScope,
  attenuation?: readonly NamespaceAttenuation[],
): NamespaceScope {
  assertScope(parent);
  if (attenuation === undefined) {
    return admitScope(parent.manifest, parent.defaultCwd, parent.grants);
  }
  if (!Array.isArray(attenuation) || attenuation.length > MAX_NAMESPACE_GRANTS) {
    throw new NamespaceError(
      "invalid_scope",
      `child namespace may contain at most ${MAX_NAMESPACE_GRANTS} grants`,
    );
  }

  const grants: NamespaceGrant[] = [];
  const keys = new Set<string>();
  for (const [index, request] of attenuation.entries()) {
    if (!isRecord(request)) {
      throw new NamespaceError("invalid_scope", `attenuation ${index} must be an object`);
    }
    const mountId = parseIdentity(request.mountId, `attenuation ${index} mountId`);
    const mount = parent.manifest.mounts.find((candidate) => candidate.mountId === mountId);
    if (mount === undefined) {
      throw new NamespaceError("access_denied", "attenuation references an inaccessible mount");
    }
    if (request.path !== undefined && typeof request.path !== "string") {
      throw new NamespaceError("invalid_scope", `attenuation ${index} path must be a string`);
    }
    const path = request.path === undefined
      ? mount.root
      : resolveNamespaceCwd(mount.root, request.path);
    if (!pathWithinNormalized(path, mount.root)) {
      throw new NamespaceError("access_denied", "attenuation path escapes its mount");
    }

    const available = effectiveRights(parent, mountId, path);
    const rights = request.rights === undefined
      ? available
      : parseRights(request.rights, `attenuation ${index}`);
    if (rights.some((right) => !available.includes(right))) {
      throw new NamespaceError("access_denied", "child namespace rights exceed its parent scope");
    }
    const key = `${mountId}\u0000${path}`;
    if (keys.has(key)) {
      throw new NamespaceError("invalid_scope", "duplicate child namespace grant");
    }
    keys.add(key);
    grants.push(Object.freeze({ mountId, path, rights }));
  }

  // Scope construction happens only after every requested grant is validated.
  // Therefore an inaccessible inherited cwd rejects the whole derivation.
  return admitScope(parent.manifest, parent.defaultCwd, grants);
}

/** Resolves and authorizes a cwd for process routing on its owning hand. */
export function routeNamespaceCwd(
  scope: NamespaceScope,
  workdir?: string,
  requiredRight: NamespaceRight = "process.exec",
): NamespaceRoute {
  assertScope(scope);
  assertRight(requiredRight, "required right");
  const cwd = resolveNamespaceCwd(scope.defaultCwd, workdir);
  const mount = resolveNamespaceMount(scope.manifest, cwd);
  if (mount === undefined) {
    throw new NamespaceError("no_execution_root", `no mount owns namespace cwd ${cwd}`);
  }
  const rights = effectiveRights(scope, mount.mountId, cwd);
  if (!rights.includes(requiredRight)) {
    throw new NamespaceError("access_denied", `namespace cwd ${cwd} lacks ${requiredRight}`);
  }
  const relativePath = cwd === mount.root ? "/" : cwd.slice(mount.root.length);
  return Object.freeze({ cwd, relativePath, mount, rights });
}

function admitScope(
  manifest: NamespaceManifest,
  defaultCwd: string,
  sourceGrants: readonly NamespaceGrant[],
): NamespaceScope {
  const grants = Object.freeze(sourceGrants.map((grant) => Object.freeze({
    mountId: grant.mountId,
    path: grant.path,
    rights: grant.rights,
  })));
  if (grants.length === 0 || !grants.some((grant) => pathWithinNormalized(defaultCwd, grant.path))) {
    throw new NamespaceError("access_denied", "child namespace cannot access its inherited cwd");
  }
  const scope = Object.freeze({ manifest, defaultCwd, grants });
  admittedScopes.add(scope);
  return scope;
}

function effectiveRights(
  scope: NamespaceScope,
  mountId: NamespaceIdentity,
  path: string,
): readonly NamespaceRight[] {
  const rights = new Set<NamespaceRight>();
  for (const grant of scope.grants) {
    if (grant.mountId === mountId && pathWithinNormalized(path, grant.path)) {
      for (const right of grant.rights) rights.add(right);
    }
  }
  return Object.freeze(NAMESPACE_RIGHTS.filter((right) => rights.has(right)));
}

function normalizePath(path: string, base: string | undefined): string {
  if (typeof path !== "string") {
    throw new NamespaceError("invalid_path", "namespace path must be a string");
  }
  assertByteLimit(path, MAX_NAMESPACE_PATH_BYTES, "namespace path", "invalid_path");
  if (CONTROL_CHARACTER.test(path) || path.includes("\\")) {
    throw new NamespaceError("invalid_path", "namespace path contains a non-portable character");
  }
  const absolute = path.startsWith("/");
  if (!absolute && base === undefined) {
    throw new NamespaceError("invalid_path", "namespace path must be absolute");
  }
  const parts = absolute ? [] : splitNormalizedBase(base!);
  for (const part of path.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") {
      if (parts.length === 0) {
        throw new NamespaceError("invalid_path", "namespace path escapes logical root");
      }
      parts.pop();
      continue;
    }
    assertByteLimit(part, MAX_NAMESPACE_SEGMENT_BYTES, "namespace path segment", "invalid_path");
    parts.push(part);
    if (parts.length > MAX_NAMESPACE_PATH_SEGMENTS) {
      throw new NamespaceError(
        "invalid_path",
        `namespace path exceeds ${MAX_NAMESPACE_PATH_SEGMENTS} segments`,
      );
    }
  }
  const normalized = parts.length === 0 ? "/" : `/${parts.join("/")}`;
  assertByteLimit(normalized, MAX_NAMESPACE_PATH_BYTES, "normalized namespace path", "invalid_path");
  return normalized;
}

function splitNormalizedBase(base: string): string[] {
  return base === "/" ? [] : base.slice(1).split("/");
}

function pathWithinNormalized(path: string, parent: string): boolean {
  return parent === "/" || path === parent || path.startsWith(`${parent}/`);
}

function parseMountRoot(value: unknown, index: number): string {
  if (typeof value !== "string" || !value.startsWith("/")) {
    throw new NamespaceError("invalid_manifest", `mount ${index} root must be absolute`);
  }
  const normalized = normalizeNamespacePath(value);
  const name = normalized.slice(1);
  if (RESERVED_ROOTS.has(name)) {
    throw new NamespaceError("invalid_manifest", `mount root ${value} is reserved`);
  }
  if (!PORTABLE_ROOT.test(name) || normalized !== value || WINDOWS_DEVICE_ROOT.test(name)) {
    throw new NamespaceError("invalid_manifest", `mount root ${value} is not a portable top-level root`);
  }
  return normalized;
}

function parseIdentity(value: unknown, label: string): NamespaceIdentity {
  if (typeof value !== "string" || value.length === 0 || value.trim() !== value
    || CONTROL_CHARACTER.test(value)) {
    throw new NamespaceError("invalid_manifest", `${label} must be a non-empty opaque identity`);
  }
  assertByteLimit(value, MAX_NAMESPACE_ID_BYTES, label, "invalid_manifest");
  return value as NamespaceIdentity;
}

function parseRights(value: unknown, label: string): readonly NamespaceRight[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > NAMESPACE_RIGHTS.length) {
    throw new NamespaceError("invalid_manifest", `${label} must contain a bounded non-empty rights set`);
  }
  const rights = new Set<NamespaceRight>();
  for (const right of value) {
    assertRight(right, label);
    if (rights.has(right)) {
      throw new NamespaceError("invalid_manifest", `${label} contains a duplicate right`);
    }
    rights.add(right);
  }
  return Object.freeze(NAMESPACE_RIGHTS.filter((right) => rights.has(right)));
}

function assertRight(value: unknown, label: string): asserts value is NamespaceRight {
  if (typeof value !== "string" || !RIGHT_SET.has(value)) {
    throw new NamespaceError("invalid_manifest", `${label} contains an unsupported namespace right`);
  }
}

function assertManifest(value: NamespaceManifest): void {
  if (!isRecord(value) || !admittedManifests.has(value)) {
    throw new NamespaceError("invalid_manifest", "namespace manifest was not admitted by this runtime");
  }
}

function assertScope(value: NamespaceScope): void {
  if (!isRecord(value) || !admittedScopes.has(value)) {
    throw new NamespaceError("invalid_scope", "namespace scope was not admitted by this runtime");
  }
}

function assertByteLimit(
  value: string,
  maximum: number,
  label: string,
  code: "invalid_manifest" | "invalid_path",
): void {
  if (utf8ByteLength(value) > maximum) {
    throw new NamespaceError(code, `${label} exceeds ${maximum} UTF-8 bytes`);
  }
}

function stableHash(value: string): string {
  let hash = 0xcbf29ce484222325n;
  for (const byte of HASH_UTF8.encode(value)) {
    hash ^= BigInt(byte);
    hash = BigInt.asUintN(64, hash * 0x100000001b3n);
  }
  return hash.toString(16).padStart(16, "0");
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
