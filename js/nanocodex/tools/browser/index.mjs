import "./browserBuffer.mjs";
import {
  createBrowserEgressFetch,
  createBrowserRuntimeFetch,
  installBrowserEgressFetch,
} from "./browserEgress.mjs";
import {
  browserAccountConnectionTool,
  browserAccountInfoTool,
  browserRuntimeInfoTool,
} from "./accountInfo.mjs";

const preparedBrowsers = new Map();
const ACCOUNT_CONNECTION_INSTRUCTIONS = "Use requestAccountConnection when the user asks to connect or authenticate GitHub, a Google Workspace service, Slack, or X. For authorization_required results, return the exact authorization_url as a Markdown link. Never claim the account is connected until a later accountInfo call reports it as authenticated.";

export {
  createOpfsGitFs,
  openOpfsGitFs,
  openOpfsWorkspaceRoot,
} from "./opfsGit.mjs";
export {
  browserThread,
  commitAndPushThread,
  initializeThreadGit,
  inspectThreadGit,
  notifyThreadGitChanged,
  pullThread,
  subscribeThreadGitChanges,
  threadGitStatus,
  withThreadGitLock,
} from "./threadGit.mjs";
export {
  getBrowserThread,
  selectBrowserThread,
  openKernelWorkspace,
  openThreadWorkspace,
  subscribeThreadWorkspaceChanges,
} from "./workspace.mjs";

export async function browser(options) {
  const prepared = options?.prepared ?? await prepareBrowser(options);
  return bindBrowser(prepared, options);
}

export function prepareBrowser(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("browser tool options must be an object");
  }
  const origin = options.origin ?? globalThis.location?.origin;
  if (typeof options.threadId !== "string" || !options.threadId) {
    throw new TypeError("browser threadId must be a non-empty string");
  }
  if (typeof origin !== "string" || !origin) {
    throw new TypeError("browser origin is required outside a browser location");
  }
  if (options.installFetch === false) {
    return prepareBrowserRuntime(options.threadId, origin, options);
  }
  const key = `${origin}\n${options.threadId}`;
  let prepared = preparedBrowsers.get(key);
  prepared ??= prepareBrowserRuntime(options.threadId, origin, options).catch((error) => {
    preparedBrowsers.delete(key);
    throw error;
  });
  preparedBrowsers.set(key, prepared);
  return prepared;
}

async function prepareBrowserRuntime(threadId, origin, options) {
  const [shellModule, standard, datasets] = await Promise.all([
    import("./browserShell.mjs"),
    import("../standard.mjs"),
    import("../dataset.mjs"),
  ]);
  const fetch = (options.installFetch === false
    ? createBrowserRuntimeFetch
    : installBrowserEgressFetch)({
    fetch: options.fetch,
    headers: options.headers,
    origin,
    threadId,
  });
  const secureFetch = createBrowserEgressFetch({ fetch, origin, threadId });
  const shell = await shellModule.prepareBrowserShell(
    threadId,
    origin,
    secureFetch,
    options.headers,
  );
  return Object.freeze({
    origin,
    threadId,
    fetch,
    shell,
    standard,
    datasets,
  });
}

export function bindBrowser(prepared, options = {}) {
  if (!prepared || typeof prepared !== "object" || Array.isArray(prepared)) {
    throw new TypeError("prepared browser runtime is required");
  }
  if (options.threadId !== undefined && options.threadId !== prepared.threadId) {
    throw new Error("prepared browser runtime belongs to a different thread");
  }
  const { datasets, fetch, shell, standard } = prepared;
  const web = {
    url: new URL("/api/tools/web-search", prepared.origin),
    fetch,
    ...options.web,
  };
  const images = {
    url: new URL("/api/tools/image-generation", prepared.origin),
    fetch,
    ...options.images,
  };
  const account = {
    endpoint: options.accountInfo?.endpoint,
    fetch,
    origin: prepared.origin,
    requireAuthorization: options.accountInfo?.requireAuthorization,
  };
  return Object.freeze({
    filesystem: shell.workspace,
    instructions: options.accountConnectionRequests
      ? `${shell.instructions}\n\n${ACCOUNT_CONNECTION_INSTRUCTIONS}`
      : shell.instructions,
    projectInstructions: shell.projectInstructions,
    tools: Object.freeze([
      standard.namedTool("exec_command", shell.execTool),
      browserRuntimeInfoTool(account, shell.descriptor),
      browserAccountInfoTool(account),
      ...(options.accountConnectionRequests ? [browserAccountConnectionTool(account)] : []),
      standard.web(web),
      standard.imageGeneration({
        ...images,
        recentImages: options.recentImages,
        rememberImage: options.rememberImage,
        workspace: shell.workspace,
      }),
      standard.viewImage({ workspace: shell.workspace }),
      standard.updatePlan(),
      datasets.dataset(options.dataset),
      shell.artifactTool,
    ]),
  });
}

export async function createBrowserBash(rawFs, thread, options) {
  const { createBrowserBash: create } = await import("./browserShell.mjs");
  return create(rawFs, thread, options);
}

export async function loadBrowserProjectInstructions(rawFs) {
  const { loadBrowserProjectInstructions: load } = await import("./browserShell.mjs");
  return load(rawFs);
}

export async function validateBrowserArtifactSource(source) {
  const { validateBrowserArtifactSource: validate } = await import("./browserShell.mjs");
  return validate(source);
}
