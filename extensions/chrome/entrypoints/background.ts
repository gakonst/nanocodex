import {
  validateCleanupInput,
  type CleanupInput,
  type OpenTabSummary,
  type PageInterrupted,
  type PageLease,
  type PageSelectionSnapshot,
  type TabClaim,
} from "../lib/extension";
import {
  compileRecipeCss,
  normalizeOrigin,
  permissionPattern,
  recipeStorageKey,
  validateRecipe,
  type SiteRecipe,
  type StoredSiteRecipe,
} from "../lib/recipe";
import {
  commitPreview,
  inspectPage,
  installPreview,
  removePersistedRecipe,
  removePreview,
} from "../lib/page";
import {
  assertCookieJarFence,
  cookieRemovalDetails,
  cookieSetDetails,
  createCookieJar,
  validateCookieJar,
  type BrowserCookieJarV1,
  type CookieCaptureHandle,
  type CookieRestoreConfirmation,
} from "../lib/cookie-sync";

const REGISTRATION_ID = "nanocodex-site-recipes-v1";
const RUNNER_FILE = "content-scripts/recipe-runner.js";
const INSTANCE_KEY = "browser-instance-id";
const LEASE_PREFIX = "page-lease:";
const SELECTION_PREFIX = "page-selection:";
const SELECTION_SET_PREFIX = "page-selection-set:";
const SELECTION_MAX_AGE_MS = 5 * 60 * 1000;
const TAB_PAGE_SIZE = 50;
const COOKIE_CONFIRMATION_MAX_AGE_MS = 60 * 1000;

interface SelectedTab {
  document_id: string;
  observed_at_ms: number;
  owner_document_id: string;
  selection_id: string;
  tab_id: number;
  url: string;
  window_id: number;
}

interface SelectionSet {
  candidates?: TabCandidate[];
  observed_at_ms: number;
  owner_document_id: string;
  selection_ids: string[];
}

interface TabCandidate {
  active: boolean;
  same_window: boolean;
  tab_id: number;
  url: string;
  window_id: number;
}

interface Lease {
  id: string;
  claim: TabClaim;
  owner_document_id: string;
  documentRevision?: string;
  previewId?: string;
  preview?: SiteRecipe;
}

interface HeldCookieJar {
  captured_at_ms: number;
  capture_id: string;
  jar: BrowserCookieJarV1;
  lease_id: string;
  owner_document_id: string;
}

interface CookieRestoreChallenge {
  capture_id: string;
  confirmation_id: string;
  created_at_ms: number;
  lease_id: string;
  owner_document_id: string;
}

const activeRequests = new Set<string>();
const cancelledRequests = new Set<string>();
const invalidatedLeases = new Set<string>();
const leaseQueues = new Map<string, Promise<unknown>>();
let recipeQueue: Promise<unknown> = Promise.resolve();
const heldCookieJars = new Map<string, HeldCookieJar>();
const cookieRestoreChallenges = new Map<string, CookieRestoreChallenge>();

export default defineBackground(() => {
  void chrome.storage.local.setAccessLevel({ accessLevel: "TRUSTED_CONTEXTS" });
  void chrome.sidePanel.setPanelBehavior({ openPanelOnActionClick: false });
  void serializeRecipes(repairRecipeRegistration);
  chrome.runtime.onStartup.addListener(() => void serializeRecipes(repairRecipeRegistration));
  chrome.runtime.onInstalled.addListener(() => void serializeRecipes(repairRecipeRegistration));
  chrome.action.onClicked.addListener((tab) => {
    void chrome.sidePanel.open({ windowId: tab.windowId });
  });
  chrome.runtime.onMessage.addListener((message: unknown, sender, sendResponse) => {
    void handleRuntimeMessage(message, sender).then(sendResponse, (error: unknown) => {
      sendResponse({ error: errorMessage(error) });
    });
    return true;
  });
  chrome.tabs.onRemoved.addListener((tabId) => {
    void interruptTab(tabId, "The selected tab was closed.");
  });
  chrome.tabs.onUpdated.addListener((tabId, change) => {
    if (change.status === "loading") {
      void interruptTab(tabId, "The selected tab navigated. Run the prompt again to inspect the new document.");
    }
  });
});

async function handleRuntimeMessage(value: unknown, sender: chrome.runtime.MessageSender): Promise<unknown> {
  const message = asRecord(value);
  if (message.type !== "recipe.for_document") requireSidePanelSender(sender);
  const ownerDocumentId = message.type === "recipe.for_document" ? undefined : requiredSenderDocumentId(sender);
  switch (message.type) {
    case "page.selection": {
      return captureTabSnapshot(false, requiredInteger(message, "window_id"), ownerDocumentId!);
    }
    case "page.tabs": {
      return captureTabSnapshot(
        true,
        requiredInteger(message, "window_id"),
        ownerDocumentId!,
        optionalNonnegativeInteger(message, "offset"),
        optionalString(message, "catalog_id"),
      );
    }
    case "page.selection.release": {
      await releaseTabSnapshot(requiredString(message, "snapshot_id"), ownerDocumentId!);
      return {};
    }
    case "page.claim": {
      const claim = await claimSelectedTab(requiredString(message, "selection_id"), ownerDocumentId!);
      const current: Lease = { id: crypto.randomUUID(), claim, owner_document_id: ownerDocumentId! };
      await saveLease(current);
      try {
        if (typeof message.previous_lease_id === "string") {
          await releaseLease(message.previous_lease_id, ownerDocumentId!);
        }
      } catch (error) {
        await chrome.storage.session.remove(leaseStorageKey(current.id));
        throw error;
      }
      return { lease_id: current.id, tab: claim } satisfies PageLease;
    }
    case "page.cleanup": {
      const leaseId = requiredString(message, "lease_id");
      const requestId = requiredString(message, "request_id");
      if (activeRequests.has(requestId)) throw new Error("The cleanup request ID is already active.");
      activeRequests.add(requestId);
      try {
        return await serializeLease(leaseId, () =>
          handleCleanup(leaseId, requestId, validateCleanupInput(message.input), ownerDocumentId!));
      } finally {
        activeRequests.delete(requestId);
        cancelledRequests.delete(requestId);
      }
    }
    case "page.cancel": {
      const requestId = requiredString(message, "request_id");
      if (activeRequests.has(requestId)) cancelledRequests.add(requestId);
      return {};
    }
    case "lease.release":
      await releaseLease(requiredString(message, "lease_id"), ownerDocumentId!);
      return {};
    case "preview.revert": {
      const leaseId = requiredString(message, "lease_id");
      await serializeLease(leaseId, async () => clearPreview(await requireLease(leaseId, ownerDocumentId!)));
      return {};
    }
    case "preview.info": {
      const current = await requireLease(requiredString(message, "lease_id"), ownerDocumentId!);
      if (!current.preview) return undefined;
      return {
        origin: current.claim.origin,
        permission: permissionPattern(current.claim.origin),
        recipe: current.preview,
      };
    }
    case "recipe.keep": {
      const leaseId = requiredString(message, "lease_id");
      return serializeLease(leaseId, async () => keepRecipe(
        await requireLease(leaseId, ownerDocumentId!),
        requiredString(message, "origin"),
      ));
    }
    case "recipe.list":
      return listRecipes();
    case "recipe.forget":
      return serializeRecipes(() => forgetRecipe(requiredString(message, "origin")));
    case "cookie.capture": {
      const leaseId = requiredString(message, "lease_id");
      return serializeLease(leaseId, () => captureCookies(leaseId, ownerDocumentId!));
    }
    case "cookie.sync.export": {
      const leaseId = requiredString(message, "lease_id");
      return serializeLease(leaseId, () => exportCookieCapture(
        requiredString(message, "capture_id"),
        leaseId,
        ownerDocumentId!,
      ));
    }
    case "cookie.capture.release":
      releaseCookieCapture(
        requiredString(message, "capture_id"),
        requiredString(message, "lease_id"),
        ownerDocumentId!,
      );
      return {};
    case "cookie.restore.stage": {
      const leaseId = requiredString(message, "lease_id");
      return serializeLease(leaseId, () => stageCookieRestore(message.jar, leaseId, ownerDocumentId!));
    }
    case "cookie.restore.prepare": {
      const leaseId = requiredString(message, "lease_id");
      return serializeLease(leaseId, () => prepareCookieRestore(
        requiredString(message, "capture_id"),
        leaseId,
        ownerDocumentId!,
      ));
    }
    case "cookie.restore.cancel":
      cancelCookieRestore(requiredString(message, "confirmation_id"), ownerDocumentId!);
      return {};
    case "cookie.restore.apply": {
      const confirmationId = requiredString(message, "confirmation_id");
      const challenge = requireCookieRestoreChallenge(confirmationId, ownerDocumentId!);
      if (message.confirmed !== true) throw new Error("Cookie restore requires explicit destructive confirmation.");
      return serializeLease(challenge.lease_id, () => applyCookieRestore(confirmationId, ownerDocumentId!));
    }
    case "recipe.for_document":
      return recipeForDocument(requiredString(message, "url"), sender);
    default:
      throw new Error("Unknown extension request.");
  }
}

async function listRecipes(): Promise<StoredSiteRecipe[]> {
  const stored = await chrome.storage.local.get(null);
  const recipes: StoredSiteRecipe[] = [];
  for (const [key, value] of Object.entries(stored)) {
    if (!key.startsWith("site-recipe:") || !value || typeof value !== "object") continue;
    try {
      const candidate = value as StoredSiteRecipe;
      const origin = normalizeOrigin(candidate.origin);
      if (!Number.isFinite(candidate.updated_at_ms)) continue;
      recipes.push({
        origin,
        recipe: validateRecipe(candidate.recipe),
        updated_at_ms: candidate.updated_at_ms,
      });
    } catch {
      // Invalid retained state is not exposed to the panel.
    }
  }
  return recipes.sort((left, right) => right.updated_at_ms - left.updated_at_ms);
}

async function captureCookies(leaseId: string, ownerDocumentId: string): Promise<CookieCaptureHandle> {
  await requireCookiesPermission();
  const { current, storeId } = await requireActiveCookieLease(leaseId, ownerDocumentId);
  const cookies = await chrome.cookies.getAll({ url: current.claim.url, storeId });
  const exact = await requireActiveCookieLease(leaseId, ownerDocumentId);
  if (exact.storeId !== storeId) throw new Error("The active tab changed browser stores during cookie capture.");
  const jar = createCookieJar({
    origin: current.claim.origin,
    profile_id: await browserInstanceId(),
    store_id: storeId,
  }, cookies);
  const captureId = crypto.randomUUID();
  purgeCookieStateForLease(leaseId);
  const held: HeldCookieJar = {
    captured_at_ms: Date.now(),
    capture_id: captureId,
    jar,
    lease_id: leaseId,
    owner_document_id: ownerDocumentId,
  };
  heldCookieJars.set(captureId, held);
  return {
    capture_id: captureId,
    lease_id: leaseId,
    origin: jar.origin,
    profile_id: jar.profile_id,
    store_id: jar.store_id,
    cookie_count: jar.cookies.length,
    captured_at_ms: held.captured_at_ms,
  };
}

async function exportCookieCapture(
  captureId: string,
  leaseId: string,
  ownerDocumentId: string,
): Promise<{ jar_id: string; jar: BrowserCookieJarV1 }> {
  const held = requireHeldCookieJar(captureId, leaseId, ownerDocumentId);
  const { current, storeId } = await requireActiveCookieLease(leaseId, ownerDocumentId);
  assertCookieJarFence(held.jar, {
    origin: current.claim.origin,
    profile_id: await browserInstanceId(),
    store_id: storeId,
  });
  return { jar_id: held.capture_id, jar: held.jar };
}

function releaseCookieCapture(captureId: string, leaseId: string, ownerDocumentId: string): void {
  requireHeldCookieJar(captureId, leaseId, ownerDocumentId);
  heldCookieJars.delete(captureId);
}

async function stageCookieRestore(
  value: unknown,
  leaseId: string,
  ownerDocumentId: string,
): Promise<CookieCaptureHandle> {
  const jar = validateCookieJar(value);
  assertSupportedCookiePartitions(jar);
  const { current, storeId } = await requireActiveCookieLease(leaseId, ownerDocumentId);
  assertCookieJarFence(jar, {
    origin: current.claim.origin,
    profile_id: await browserInstanceId(),
    store_id: storeId,
  });
  const captureId = crypto.randomUUID();
  purgeCookieStateForLease(leaseId);
  const held: HeldCookieJar = {
    captured_at_ms: Date.now(),
    capture_id: captureId,
    jar,
    lease_id: leaseId,
    owner_document_id: ownerDocumentId,
  };
  heldCookieJars.set(captureId, held);
  return {
    capture_id: captureId,
    lease_id: leaseId,
    origin: jar.origin,
    profile_id: jar.profile_id,
    store_id: jar.store_id,
    cookie_count: jar.cookies.length,
    captured_at_ms: held.captured_at_ms,
  };
}

async function prepareCookieRestore(
  captureId: string,
  leaseId: string,
  ownerDocumentId: string,
): Promise<CookieRestoreConfirmation> {
  await requireCookiesPermission();
  cleanupExpiredCookieChallenges();
  const held = requireHeldCookieJar(captureId, leaseId, ownerDocumentId);
  const { current, storeId } = await requireActiveCookieLease(leaseId, ownerDocumentId);
  assertCookieJarFence(held.jar, {
    origin: current.claim.origin,
    profile_id: await browserInstanceId(),
    store_id: storeId,
  });
  const confirmationId = crypto.randomUUID();
  cookieRestoreChallenges.set(confirmationId, {
    capture_id: captureId,
    confirmation_id: confirmationId,
    created_at_ms: Date.now(),
    lease_id: leaseId,
    owner_document_id: ownerDocumentId,
  });
  return {
    confirmation_id: confirmationId,
    origin: held.jar.origin,
    cookie_count: held.jar.cookies.length,
  };
}

function cancelCookieRestore(confirmationId: string, ownerDocumentId: string): void {
  const challenge = cookieRestoreChallenges.get(confirmationId);
  if (challenge?.owner_document_id === ownerDocumentId) {
    cookieRestoreChallenges.delete(confirmationId);
    const held = heldCookieJars.get(challenge.capture_id);
    if (held?.owner_document_id === ownerDocumentId && held.lease_id === challenge.lease_id) {
      heldCookieJars.delete(challenge.capture_id);
    }
  }
}

async function applyCookieRestore(
  confirmationId: string,
  ownerDocumentId: string,
): Promise<{ origin: string; cookie_count: number }> {
  await requireCookiesPermission();
  const challenge = requireCookieRestoreChallenge(confirmationId, ownerDocumentId);
  cookieRestoreChallenges.delete(confirmationId);
  const held = requireHeldCookieJar(challenge.capture_id, challenge.lease_id, ownerDocumentId);
  assertSupportedCookiePartitions(held.jar);
  const { current, storeId } = await requireActiveCookieLease(challenge.lease_id, ownerDocumentId);
  const fence = {
    origin: current.claim.origin,
    profile_id: await browserInstanceId(),
    store_id: storeId,
  };
  assertCookieJarFence(held.jar, fence);

  const currentCookies = await chrome.cookies.getAll({ url: current.claim.url, storeId });
  const backup = createCookieJar(fence, currentCookies);
  await requireActiveCookieLease(challenge.lease_id, ownerDocumentId);
  try {
    await replaceCookiesForOrigin(backup, held.jar, current.claim.url);
  } catch (cause) {
    try {
      const partial = createCookieJar(fence, await chrome.cookies.getAll({ url: current.claim.url, storeId }));
      await replaceCookiesForOrigin(partial, backup, current.claim.url);
    } catch {
      throw new Error(`Cookie restore failed and the previous site cookies could not be fully recovered. ${errorMessage(cause)}`);
    }
    throw cause;
  }
  await requireActiveCookieLease(challenge.lease_id, ownerDocumentId);
  heldCookieJars.delete(held.capture_id);
  return { origin: held.jar.origin, cookie_count: held.jar.cookies.length };
}

async function replaceCookiesForOrigin(
  existing: BrowserCookieJarV1,
  replacement: BrowserCookieJarV1,
  leasedUrl: string,
): Promise<void> {
  assertCookieJarFence(replacement, existing);
  for (const cookie of existing.cookies) {
    await chrome.cookies.remove(cookieRemovalDetails(cookie, existing.origin));
  }
  for (const cookie of replacement.cookies) {
    const restored = await chrome.cookies.set(cookieSetDetails(cookie, replacement.origin));
    if (!restored) throw new Error("Chrome rejected a cookie during restore.");
  }
  const url = new URL(leasedUrl);
  if (url.origin !== replacement.origin) throw new Error("The leased origin changed during cookie restore.");
}

async function requireCookiesPermission(): Promise<void> {
  if (!await chrome.permissions.contains({ permissions: ["cookies"] })) {
    throw new Error("Cookie access was not granted from the side panel.");
  }
}

async function requireActiveCookieLease(
  leaseId: string,
  ownerDocumentId: string,
): Promise<{ current: Lease; storeId: string }> {
  const current = await requireLease(leaseId, ownerDocumentId);
  await assertLeaseDocument(current);
  const tab = await chrome.tabs.get(current.claim.tab_id);
  if (tab.incognito) throw new Error("Cookie sync is unavailable in incognito windows.");
  if (!tab.active || tab.windowId !== current.claim.window_id || !tab.url
    || normalizeOrigin(tab.url) !== current.claim.origin || !/^https?:\/\//.test(tab.url)) {
    throw new Error("Cookie sync requires the exact currently leased active HTTP(S) tab.");
  }
  const stores = (await chrome.cookies.getAllCookieStores())
    .filter((store) => store.tabIds.includes(current.claim.tab_id));
  if (stores.length !== 1 || !stores[0]?.id) throw new Error("The active tab's cookie store is unavailable.");
  await assertLeaseDocument(current);
  return { current, storeId: stores[0].id };
}

function requireHeldCookieJar(captureId: string, leaseId: string, ownerDocumentId: string): HeldCookieJar {
  const held = heldCookieJars.get(captureId);
  if (!held || held.lease_id !== leaseId || held.owner_document_id !== ownerDocumentId) {
    throw new Error("The in-memory cookie capture expired. Capture this site again.");
  }
  return held;
}

function requireCookieRestoreChallenge(
  confirmationId: string,
  ownerDocumentId: string,
): CookieRestoreChallenge {
  cleanupExpiredCookieChallenges();
  const challenge = cookieRestoreChallenges.get(confirmationId);
  if (!challenge || challenge.owner_document_id !== ownerDocumentId) {
    throw new Error("The cookie restore confirmation expired.");
  }
  return challenge;
}

function cleanupExpiredCookieChallenges(): void {
  const now = Date.now();
  for (const [id, challenge] of cookieRestoreChallenges) {
    if (now - challenge.created_at_ms > COOKIE_CONFIRMATION_MAX_AGE_MS) cookieRestoreChallenges.delete(id);
  }
}

function purgeCookieStateForLease(leaseId: string): void {
  for (const [id, held] of heldCookieJars) {
    if (held.lease_id === leaseId) heldCookieJars.delete(id);
  }
  for (const [id, challenge] of cookieRestoreChallenges) {
    if (challenge.lease_id === leaseId) cookieRestoreChallenges.delete(id);
  }
}

function assertSupportedCookiePartitions(jar: BrowserCookieJarV1): void {
  const partitioned = jar.cookies.filter((cookie) => cookie.partitionKey);
  if (partitioned.length === 0) return;
  const match = /(?:Chrome|Chromium)\/(\d+)/.exec(navigator.userAgent);
  const major = match ? Number(match[1]) : 0;
  if (major < 119) throw new Error("This Chrome version cannot restore partitioned cookies.");
  if (major < 130 && partitioned.some((cookie) => cookie.partitionKey?.hasCrossSiteAncestor !== undefined)) {
    throw new Error("This Chrome version cannot preserve the cookie partition ancestor state.");
  }
}

async function forgetRecipe(originValue: string): Promise<{ forgotten: boolean }> {
  const origin = normalizeOrigin(originValue);
  const key = recipeStorageKey(origin);
  const stored = await chrome.storage.local.get(key);
  if (!stored[key]) return { forgotten: false };
  const pattern = permissionPattern(origin);
  await chrome.storage.local.remove(key);
  try {
    await repairRecipeRegistration();
  } catch (error) {
    await chrome.storage.local.set({ [key]: stored[key] });
    await repairRecipeRegistration().catch(() => {});
    throw error;
  }
  await removeRecipeFromOpenTabs(pattern);
  return { forgotten: true };
}

async function handleCleanup(
  leaseId: string,
  requestId: string,
  input: CleanupInput,
  ownerDocumentId: string,
): Promise<unknown> {
  const current = await requireLease(leaseId, ownerDocumentId);
  throwIfCancelled(requestId);
  await assertLeaseDocument(current);
  throwIfCancelled(requestId);
  switch (input.action) {
    case "list_tabs":
      throw new Error("Open tabs must be listed before a page lease is claimed.");
    case "inspect": {
      const [injection] = await chrome.scripting.executeScript({
        target: { tabId: current.claim.tab_id, documentIds: [current.claim.document_id] },
        world: "ISOLATED",
        func: inspectPage,
      });
      throwIfCancelled(requestId);
      if (!injection?.result) throw new Error("The selected document could not be inspected.");
      current.documentRevision = injection.result.document_revision;
      await saveLease(current);
      return injection.result;
    }
    case "preview": {
      if (!current.documentRevision || input.document_revision !== current.documentRevision) {
        throw new Error("The inspected document revision is stale.");
      }
      const recipe = validateRecipe(input.recipe);
      for (const selector of recipe.hide_selectors) {
        await validateSelector(current.claim, selector);
        throwIfCancelled(requestId);
      }
      const css = compileRecipeCss(recipe);
      await chrome.scripting.executeScript({
        target: { tabId: current.claim.tab_id, documentIds: [current.claim.document_id] },
        world: "ISOLATED",
        func: installPreview,
        args: [css],
      });
      if (cancelledRequests.has(requestId)) {
        await clearPreview(current);
        throw new Error("The cleanup request was cancelled.");
      }
      current.preview = recipe;
      current.previewId = crypto.randomUUID();
      await saveLease(current);
      if (cancelledRequests.has(requestId)) {
        await clearPreview(current);
        throw new Error("The cleanup request was cancelled.");
      }
      return { previewed: true, preview_id: current.previewId, name: recipe.name };
    }
    case "revert_preview":
      if (input.preview_id !== current.previewId) {
        throw new Error("The preview ID does not match the active preview.");
      }
      await clearPreview(current);
      return { reverted: true };
  }
}

async function keepRecipe(current: Lease, originValue: string): Promise<{ name: string }> {
  if (!current.preview) throw new Error("There is no preview to keep.");
  const origin = normalizeOrigin(originValue);
  if (origin !== current.claim.origin) throw new Error("The selected tab changed before the recipe was saved.");
  const pattern = permissionPattern(origin);
  if (!await chrome.permissions.contains({ origins: [pattern] })) {
    throw new Error("Site access was not granted.");
  }
  const stored: StoredSiteRecipe = { origin, recipe: current.preview, updated_at_ms: Date.now() };
  const key = recipeStorageKey(origin);
  await serializeRecipes(async () => {
    const previous = await chrome.storage.local.get(key);
    await chrome.storage.local.set({ [key]: stored });
    try {
      await repairRecipeRegistration();
    } catch (error) {
      if (previous[key] === undefined) await chrome.storage.local.remove(key);
      else await chrome.storage.local.set({ [key]: previous[key] });
      await repairRecipeRegistration().catch(() => {});
      throw error;
    }
  });
  const css = compileRecipeCss(current.preview);
  await applyRecipeToOpenTabs(pattern, css);
  delete current.preview;
  delete current.previewId;
  await saveLease(current);
  return { name: stored.recipe.name };
}

async function captureTabSnapshot(
  includeAll: boolean,
  ownerWindowId: number,
  ownerDocumentId: string,
  offset = 0,
  catalogId?: string,
): Promise<PageSelectionSnapshot> {
  await cleanupExpiredSelections();
  let existingSet: SelectionSet | undefined;
  let candidates: TabCandidate[];
  if (catalogId) {
    const stored = await chrome.storage.session.get(selectionSetStorageKey(catalogId));
    const value = stored[selectionSetStorageKey(catalogId)];
    if (!isSelectionSet(value)
      || value.owner_document_id !== ownerDocumentId
      || !value.candidates
      || Date.now() - value.observed_at_ms > SELECTION_MAX_AGE_MS) {
      throw new Error("The open-tab catalog expired. List tabs again from the beginning.");
    }
    existingSet = value;
    candidates = value.candidates;
  } else {
    const [activeTabs, ownerWindow] = await Promise.all([
      chrome.tabs.query({ active: true, windowId: ownerWindowId }),
      chrome.windows.get(ownerWindowId),
    ]);
    const eligible = (tab: chrome.tabs.Tab) => isEligibleTab(tab) && tab.incognito === ownerWindow.incognito;
    const active = activeTabs.find(eligible);
    let tabs: chrome.tabs.Tab[];
    if (includeAll) {
      tabs = (await chrome.tabs.query({})).filter(eligible);
    } else if (active) {
      tabs = [active];
    } else {
      tabs = [];
    }
    const ordered = active
      ? [active, ...tabs.filter((tab) => tab.id !== active.id)]
      : tabs;
    candidates = ordered.flatMap((tab) => tab.id === undefined || !tab.url ? [] : [{
      active: tab.active,
      same_window: tab.windowId === ownerWindowId,
      tab_id: tab.id,
      url: tab.url,
      window_id: tab.windowId,
    }]);
  }
  const page = includeAll ? candidates.slice(offset, offset + TAB_PAGE_SIZE) : candidates;
  const snapshotId = catalogId ?? crypto.randomUUID();
  const entries: Record<string, unknown> = {};
  const summaries: OpenTabSummary[] = [];
  const selectionIds: string[] = [];
  const exactTabs = await Promise.all(page.map(async (candidate) => ({
    candidate,
    tab: await currentCandidateTab(candidate).catch(() => undefined),
  })));
  for (const { candidate, tab } of exactTabs) {
    if (!tab?.url) continue;
    const exact = await exactTabDocument(tab).catch(() => undefined);
    if (!exact) continue;
    const selectionId = crypto.randomUUID();
    const target: SelectedTab = {
      document_id: exact.document_id,
      observed_at_ms: Date.now(),
      owner_document_id: ownerDocumentId,
      selection_id: selectionId,
      tab_id: candidate.tab_id,
      window_id: candidate.window_id,
      url: exact.url,
    };
    entries[selectionStorageKey(selectionId)] = target;
    selectionIds.push(selectionId);
    summaries.push({
      tab_ref: selectionId,
      title: boundedTabTitle(tab.title, tab.url),
      origin: normalizeOrigin(exact.url),
      url: visibleTabUrl(exact.url),
      active: candidate.active,
      same_window: candidate.same_window,
    });
  }
  entries[selectionSetStorageKey(snapshotId)] = {
    ...(includeAll ? { candidates } : {}),
    observed_at_ms: Date.now(),
    owner_document_id: ownerDocumentId,
    selection_ids: [...(existingSet?.selection_ids ?? []), ...selectionIds],
  } satisfies SelectionSet;
  await chrome.storage.session.set(entries);
  return {
    snapshot_id: snapshotId,
    ...(offset === 0 && summaries[0]?.same_window && summaries[0].active
      ? { default_tab_ref: summaries[0].tab_ref }
      : {}),
    ...(includeAll && offset + page.length < candidates.length ? { next_offset: offset + page.length } : {}),
    tabs: summaries,
  };
}

async function currentCandidateTab(candidate: TabCandidate): Promise<chrome.tabs.Tab | undefined> {
  const tab = await chrome.tabs.get(candidate.tab_id);
  if (tab.windowId !== candidate.window_id || tab.url !== candidate.url || !isEligibleTab(tab)) return undefined;
  return tab;
}

async function cleanupExpiredSelections(): Promise<void> {
  const now = Date.now();
  const stored = await chrome.storage.session.get(null);
  const expired = Object.entries(stored).flatMap(([key, value]) => {
    if (key.startsWith(SELECTION_PREFIX)
      && isSelectedTab(value)
      && now - value.observed_at_ms > SELECTION_MAX_AGE_MS) return [key];
    if (key.startsWith(SELECTION_SET_PREFIX)
      && isSelectionSet(value)
      && now - value.observed_at_ms > SELECTION_MAX_AGE_MS) return [key];
    return [];
  });
  if (expired.length > 0) await chrome.storage.session.remove(expired);
}

async function releaseTabSnapshot(snapshotId: string, ownerDocumentId: string): Promise<void> {
  const key = selectionSetStorageKey(snapshotId);
  const stored = await chrome.storage.session.get(key);
  const selectionSet = stored[key];
  if (!isSelectionSet(selectionSet) || selectionSet.owner_document_id !== ownerDocumentId) return;
  const selectionIds = selectionSet.selection_ids;
  await chrome.storage.session.remove([key, ...selectionIds.map(selectionStorageKey)]);
}

function isEligibleTab(tab: chrome.tabs.Tab): boolean {
  return tab.id !== undefined
    && typeof tab.url === "string"
    && /^https?:\/\//.test(tab.url)
    && tab.status === "complete"
    && tab.discarded !== true
    && !tab.pendingUrl;
}

async function exactTabDocument(tab: chrome.tabs.Tab): Promise<{ document_id: string; url: string } | undefined> {
  if (tab.id === undefined || !tab.url) return undefined;
  const [probe] = await chrome.scripting.executeScript({
    target: { tabId: tab.id, frameIds: [0] },
    world: "ISOLATED",
    func: () => location.href,
  });
  if (!probe?.documentId || typeof probe.result !== "string" || probe.result !== tab.url) return undefined;
  return { document_id: probe.documentId, url: probe.result };
}

function boundedTabTitle(title: string | undefined, url: string): string {
  const value = title?.trim() || new URL(url).hostname;
  return value.length > 160 ? `${value.slice(0, 159)}…` : value;
}

function visibleTabUrl(value: string): string {
  const url = new URL(value);
  const visible = `${url.origin}${url.pathname}`;
  return visible.length > 512 ? `${visible.slice(0, 511)}…` : visible;
}

async function claimSelectedTab(selectionId: string, ownerDocumentId: string): Promise<TabClaim> {
  const stored = await chrome.storage.session.get(selectionStorageKey(selectionId));
  const target = stored[selectionStorageKey(selectionId)] as SelectedTab | undefined;
  if (!isSelectedTab(target)
    || target.selection_id !== selectionId
    || target.owner_document_id !== ownerDocumentId
    || Date.now() - target.observed_at_ms > SELECTION_MAX_AGE_MS) {
    throw new Error("The open-tab selection expired. List tabs again or send the request again.");
  }
  const tab = await chrome.tabs.get(target.tab_id);
  if (tab.windowId !== target.window_id || tab.url !== target.url) {
    throw new Error("The selected tab navigated or closed after it was chosen. Send the request again.");
  }
  const origin = normalizeOrigin(tab.url);
  let probe: chrome.scripting.InjectionResult<string> | undefined;
  try {
    [probe] = await chrome.scripting.executeScript({
      target: { tabId: target.tab_id, documentIds: [target.document_id] },
      world: "ISOLATED",
      func: () => location.href,
    });
  } catch {
    throw new Error("Nanocodex no longer has access to that page.");
  }
  if (probe?.documentId !== target.document_id
    || String(probe.result) !== target.url
    || normalizeOrigin(String(probe.result)) !== origin) {
    throw new Error("The selected tab changed while it was being claimed.");
  }
  return {
    browser_instance_id: await browserInstanceId(),
    window_id: target.window_id,
    tab_id: target.tab_id,
    document_id: probe.documentId,
    origin,
    title: boundedTabTitle(tab.title, tab.url),
    url: String(probe.result),
    ...(tab.groupId !== undefined && tab.groupId >= 0 ? { group_id: tab.groupId } : {}),
    observed_at_ms: Date.now(),
  };
}

async function assertLeaseDocument(current: Lease): Promise<void> {
  const tab = await chrome.tabs.get(current.claim.tab_id);
  if (
    tab.windowId !== current.claim.window_id
    || !tab.url
    || tab.url !== current.claim.url
  ) {
    throw new Error("The leased document changed. Run the prompt again to claim the current page.");
  }
  const [probe] = await chrome.scripting.executeScript({
    target: { tabId: current.claim.tab_id, documentIds: [current.claim.document_id] },
    world: "ISOLATED",
    func: () => location.href,
  });
  if (probe?.documentId !== current.claim.document_id || probe.result !== current.claim.url) {
    throw new Error("The leased document changed. Run the prompt again to claim the current page.");
  }
}

async function browserInstanceId(): Promise<string> {
  const stored = await chrome.storage.local.get(INSTANCE_KEY);
  if (typeof stored[INSTANCE_KEY] === "string") return stored[INSTANCE_KEY];
  const id = crypto.randomUUID();
  await chrome.storage.local.set({ [INSTANCE_KEY]: id });
  return id;
}

async function requireLease(leaseId: string, ownerDocumentId: string): Promise<Lease> {
  const stored = await chrome.storage.session.get(leaseStorageKey(leaseId));
  const value = stored[leaseStorageKey(leaseId)] as Lease | undefined;
  if (!value || value.id !== leaseId || value.owner_document_id !== ownerDocumentId) {
    throw new Error("The selected-page lease expired.");
  }
  return value;
}

async function saveLease(current: Lease): Promise<void> {
  if (invalidatedLeases.has(current.id)) throw new Error("The selected-page lease expired.");
  await chrome.storage.session.set({ [leaseStorageKey(current.id)]: current });
}

async function releaseLease(leaseId: string, ownerDocumentId?: string): Promise<void> {
  await serializeLease(leaseId, async () => {
    const stored = await chrome.storage.session.get(leaseStorageKey(leaseId));
    const current = stored[leaseStorageKey(leaseId)] as Lease | undefined;
    if (!current || current.id !== leaseId) {
      purgeCookieStateForLease(leaseId);
      return;
    }
    if (ownerDocumentId && current.owner_document_id !== ownerDocumentId) {
      throw new Error("The selected-page lease expired.");
    }
    try {
      await clearPreview(current);
    } catch (error) {
      if (!invalidatedLeases.has(leaseId)) throw error;
    }
    await chrome.storage.session.remove(leaseStorageKey(leaseId));
    purgeCookieStateForLease(leaseId);
  });
}

async function validateSelector(claim: TabClaim, selector: string): Promise<void> {
  const [result] = await chrome.scripting.executeScript({
    target: { tabId: claim.tab_id, documentIds: [claim.document_id] },
    world: "ISOLATED",
    func: (candidate: string) => {
      try { document.querySelector(candidate); return true; } catch { return false; }
    },
    args: [selector],
  });
  if (result?.result !== true) throw new Error(`Invalid selector: ${selector}`);
}

async function clearPreview(current: Lease): Promise<void> {
  try {
    await chrome.scripting.executeScript({
      target: { tabId: current.claim.tab_id, documentIds: [current.claim.document_id] },
      world: "ISOLATED",
      func: removePreview,
    });
  } catch {
    // A closed or navigated document already discarded the preview.
  } finally {
    delete current.preview;
    delete current.previewId;
    await saveLease(current);
  }
}

async function interruptTab(tabId: number, reason: string): Promise<void> {
  const stored = await chrome.storage.session.get(null);
  const interrupted = Object.entries(stored)
    .filter(([key, value]) => key.startsWith(LEASE_PREFIX) && isLease(value) && value.claim.tab_id === tabId)
    .map(([, value]) => value as Lease);
  for (const current of interrupted) {
    invalidatedLeases.add(current.id);
    await chrome.storage.session.remove(leaseStorageKey(current.id));
    purgeCookieStateForLease(current.id);
    const message: PageInterrupted = { type: "page.interrupted", lease_id: current.id, reason };
    void chrome.runtime.sendMessage(message).catch(() => {});
  }
}

async function serializeLease<Result>(leaseId: string, operation: () => Promise<Result>): Promise<Result> {
  const previous = leaseQueues.get(leaseId) ?? Promise.resolve();
  const current = previous.catch(() => {}).then(operation);
  leaseQueues.set(leaseId, current);
  try {
    return await current;
  } finally {
    if (leaseQueues.get(leaseId) === current) leaseQueues.delete(leaseId);
  }
}

async function serializeRecipes<Result>(operation: () => Promise<Result>): Promise<Result> {
  const current = recipeQueue.catch(() => {}).then(operation);
  recipeQueue = current;
  return current;
}

async function applyRecipeToOpenTabs(pattern: string, css: string): Promise<void> {
  const tabs = await chrome.tabs.query({ url: pattern });
  await Promise.all(tabs.flatMap((tab) => tab.id === undefined ? [] : [
    chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "ISOLATED",
      func: commitPreview,
      args: [css],
    }).catch(() => []),
  ]));
}

async function removeRecipeFromOpenTabs(pattern: string): Promise<void> {
  const tabs = await chrome.tabs.query({ url: pattern });
  await Promise.all(tabs.flatMap((tab) => tab.id === undefined ? [] : [
    chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds: [0] },
      world: "ISOLATED",
      func: removePersistedRecipe,
    }).catch(() => []),
  ]));
}

function throwIfCancelled(requestId: string): void {
  if (cancelledRequests.has(requestId)) throw new Error("The cleanup request was cancelled.");
}

function leaseStorageKey(leaseId: string): string {
  return `${LEASE_PREFIX}${leaseId}`;
}

function selectionStorageKey(selectionId: string): string {
  return `${SELECTION_PREFIX}${selectionId}`;
}

function selectionSetStorageKey(snapshotId: string): string {
  return `${SELECTION_SET_PREFIX}${snapshotId}`;
}

function isLease(value: unknown): value is Lease {
  return Boolean(value) && typeof value === "object" && typeof (value as Lease).id === "string"
    && Boolean((value as Lease).claim) && typeof (value as Lease).claim.tab_id === "number";
}

function isSelectedTab(value: unknown): value is SelectedTab {
  return Boolean(value) && typeof value === "object"
    && typeof (value as SelectedTab).document_id === "string"
    && Number.isFinite((value as SelectedTab).observed_at_ms)
    && typeof (value as SelectedTab).owner_document_id === "string"
    && typeof (value as SelectedTab).selection_id === "string"
    && typeof (value as SelectedTab).tab_id === "number"
    && typeof (value as SelectedTab).window_id === "number"
    && typeof (value as SelectedTab).url === "string";
}

function isSelectionSet(value: unknown): value is SelectionSet {
  return Boolean(value) && typeof value === "object"
    && Number.isFinite((value as SelectionSet).observed_at_ms)
    && typeof (value as SelectionSet).owner_document_id === "string"
    && ((value as SelectionSet).candidates === undefined
      || (Array.isArray((value as SelectionSet).candidates)
        && (value as SelectionSet).candidates!.every(isTabCandidate)))
    && Array.isArray((value as SelectionSet).selection_ids)
    && (value as SelectionSet).selection_ids.every((entry) => typeof entry === "string");
}

function isTabCandidate(value: unknown): value is TabCandidate {
  return Boolean(value) && typeof value === "object"
    && typeof (value as TabCandidate).active === "boolean"
    && typeof (value as TabCandidate).same_window === "boolean"
    && typeof (value as TabCandidate).tab_id === "number"
    && typeof (value as TabCandidate).url === "string"
    && typeof (value as TabCandidate).window_id === "number";
}

async function recipeForDocument(urlValue: string, sender: chrome.runtime.MessageSender): Promise<{ css: string } | undefined> {
  if (sender.frameId !== 0 || !sender.tab?.url || normalizeOrigin(sender.tab.url) !== normalizeOrigin(urlValue)) return undefined;
  const origin = normalizeOrigin(urlValue);
  const stored = await chrome.storage.local.get(recipeStorageKey(origin));
  const value = stored[recipeStorageKey(origin)] as StoredSiteRecipe | undefined;
  if (!value || value.origin !== origin) return undefined;
  return { css: compileRecipeCss(validateRecipe(value.recipe)) };
}

async function repairRecipeRegistration(): Promise<void> {
  const stored = await chrome.storage.local.get(null);
  const matches = new Set<string>();
  for (const [key, value] of Object.entries(stored)) {
    if (!key.startsWith("site-recipe:") || !value || typeof value !== "object") continue;
    try {
      const origin = normalizeOrigin((value as StoredSiteRecipe).origin);
      validateRecipe((value as StoredSiteRecipe).recipe);
      const pattern = permissionPattern(origin);
      if (await chrome.permissions.contains({ origins: [pattern] })) matches.add(pattern);
    } catch {
      // Invalid application state is ignored rather than broadening site access.
    }
  }
  const [registered] = await chrome.scripting.getRegisteredContentScripts({ ids: [REGISTRATION_ID] });
  if (matches.size === 0) {
    if (registered) await chrome.scripting.unregisterContentScripts({ ids: [REGISTRATION_ID] });
    return;
  }
  const desired: chrome.scripting.RegisteredContentScript = {
    id: REGISTRATION_ID,
    matches: [...matches].sort(),
    js: [RUNNER_FILE],
    runAt: "document_start",
    persistAcrossSessions: true,
    world: "ISOLATED",
  };
  if (!registered) {
    await chrome.scripting.registerContentScripts([desired]);
    return;
  }
  const currentMatches = [...(registered.matches ?? [])].sort();
  if (JSON.stringify(currentMatches) !== JSON.stringify(desired.matches)) {
    await chrome.scripting.updateContentScripts([desired]);
  }
}

function asRecord(value: unknown): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
}

function requiredString(record: Record<string, unknown>, key: string): string {
  if (typeof record[key] !== "string" || !record[key]) throw new Error(`${key} must be a non-empty string`);
  return record[key];
}

function requiredInteger(record: Record<string, unknown>, key: string): number {
  const value = record[key];
  if (!Number.isInteger(value)) throw new Error(`${key} must be an integer`);
  return value as number;
}

function optionalString(record: Record<string, unknown>, key: string): string | undefined {
  return record[key] === undefined ? undefined : requiredString(record, key);
}

function optionalNonnegativeInteger(record: Record<string, unknown>, key: string): number {
  if (record[key] === undefined) return 0;
  const value = requiredInteger(record, key);
  if (value < 0) throw new Error(`${key} must not be negative`);
  return value;
}

function requiredSenderDocumentId(sender: chrome.runtime.MessageSender): string {
  if (typeof sender.documentId !== "string" || !sender.documentId) {
    throw new Error("The side-panel document identity is unavailable.");
  }
  return sender.documentId;
}

function requireSidePanelSender(sender: chrome.runtime.MessageSender): void {
  if (sender.id !== chrome.runtime.id || sender.url !== chrome.runtime.getURL("sidepanel.html")) {
    throw new Error("This extension request is restricted to the Nanocodex side panel.");
  }
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
