import "./browserBuffer.mjs";
import git from "isomorphic-git";
import http from "isomorphic-git/http/web";
import { createOpfsGitFs, openOpfsGitFs, openOpfsWorkspaceRoot } from "./opfsGit.mjs";
export const THREAD_GIT_DIRECTORY = "/workspace";
export const THREAD_GIT_AUTHOR = { name: "Nanocodex", email: "agent@nanocodex.dev" };
const directory = THREAD_GIT_DIRECTORY;
const author = THREAD_GIT_AUTHOR;
const configuredRemotePath = "nanocodex.remote";
export function browserThread(threadId, origin) {
    if (!/^[a-f0-9]{8}-[a-f0-9]{4}-[1-5][a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/.test(threadId)) {
        throw new Error("invalid browser thread id");
    }
    const baseUrl = new URL(origin);
    const shareUrl = new URL("/", baseUrl);
    shareUrl.searchParams.set("thread", threadId);
    return {
        id: threadId,
        workspaceName: `nanocodex-thread-${threadId}`,
        repositoryName: `thread-${threadId}`,
        branch: "nanocodex",
        remoteUrl: `${baseUrl.origin}/git/thread-${threadId}`,
        shareUrl: shareUrl.toString(),
    };
}
export async function initializeThreadGit(thread) {
    return inspectThreadGit(thread, async (fs) => {
        return status(fs, thread);
    });
}
/** Ensures the repository exists without scanning its complete worktree. */
export function prepareThreadGit(thread) {
    return inspectPreparedThreadGit(thread, (rawFs, workspaceRoot) => ({ rawFs, workspaceRoot }));
}
export function inspectThreadGit(thread, inspect) {
    return inspectPreparedThreadGit(thread, (rawFs) => inspect(rawFs));
}
async function inspectPreparedThreadGit(thread, inspect) {
    return withThreadGitLock(thread, async () => {
        const workspaceRoot = await openOpfsWorkspaceRoot(thread.workspaceName);
        const fs = createOpfsGitFs(workspaceRoot);
        if (!(await exists(fs, `${directory}/.git/config`))) {
            await initializeOrRestore(fs, thread);
        }
        await configureRemote(fs, thread);
        return inspect(fs, workspaceRoot);
    });
}
export async function threadGitStatus(thread) {
    return withThreadGitLock(thread, async () => {
        const fs = await openOpfsGitFs(thread.workspaceName);
        if (!(await exists(fs, `${directory}/.git/config`)))
            await initializeOrRestore(fs, thread);
        return status(fs, thread);
    });
}
export async function commitAndPushThread(thread, message = "Update workspace", notificationSource) {
    return withThreadGitLock(thread, async () => {
        const fs = await openOpfsGitFs(thread.workspaceName);
        if (!(await exists(fs, `${directory}/.git/config`)))
            await initializeOrRestore(fs, thread);
        await configureRemote(fs, thread);
        const matrix = await git.statusMatrix({ fs, dir: directory });
        const changed = matrix.filter(([, headStatus, workdirStatus]) => headStatus !== workdirStatus);
        for (const [filepath, , workdirStatus] of changed) {
            if (workdirStatus === 0)
                await git.remove({ fs, dir: directory, filepath });
            else
                await git.add({ fs, dir: directory, filepath });
        }
        if (changed.length > 0) {
            await git.commit({ fs, dir: directory, message: message.trim() || "Update workspace", author });
        }
        const head = await resolveHead(fs);
        if (!head)
            throw new Error("Create at least one workspace file before the first push");
        await git.push({
            fs,
            http,
            dir: directory,
            remote: "origin",
            ref: thread.branch,
            remoteRef: thread.branch,
        });
        const next = await status(fs, thread);
        notifyThreadGitChanged(thread, notificationSource);
        return next;
    });
}
export async function pullThread(thread, notificationSource) {
    return withThreadGitLock(thread, async () => {
        const fs = await openOpfsGitFs(thread.workspaceName);
        if (!(await exists(fs, `${directory}/.git/config`)))
            await initializeOrRestore(fs, thread);
        await configureRemote(fs, thread);
        const refs = await remoteRefs(thread);
        if (!refs.some((ref) => ref.ref === thread.branch || ref.ref === `refs/heads/${thread.branch}`)) {
            throw new Error("This thread has not been pushed yet");
        }
        if (await resolveHead(fs)) {
            await git.pull({ fs, http, dir: directory, remote: "origin", ref: thread.branch, author });
        }
        else {
            await restoreRemote(fs, thread);
        }
        const next = await status(fs, thread);
        notifyThreadGitChanged(thread, notificationSource);
        return next;
    });
}
export function subscribeThreadGitChanges(thread, listener) {
    if (typeof BroadcastChannel === "undefined")
        return () => undefined;
    const channel = new BroadcastChannel(`nanocodex-git-${thread.id}`);
    channel.addEventListener("message", (event) => {
        const source = event.data != null && typeof event.data === "object" &&
            typeof event.data.source === "string"
            ? event.data.source
            : undefined;
        listener(source);
    });
    return () => channel.close();
}
export function notifyThreadGitChanged(thread, source) {
    if (typeof BroadcastChannel === "undefined")
        return;
    const channel = new BroadcastChannel(`nanocodex-git-${thread.id}`);
    channel.postMessage({ type: "changed", source });
    channel.close();
}
async function initializeOrRestore(fs, thread) {
    const refs = await remoteRefs(thread);
    await git.init({ fs, dir: directory, defaultBranch: thread.branch });
    await configureRemote(fs, thread);
    if (refs.some((ref) => ref.ref === thread.branch || ref.ref === `refs/heads/${thread.branch}`)) {
        await restoreRemote(fs, thread);
    }
}
async function restoreRemote(fs, thread) {
    const fetched = await git.fetch({
        fs,
        http,
        dir: directory,
        remote: "origin",
        ref: thread.branch,
        singleBranch: true,
    });
    const oid = fetched.fetchHead;
    if (!oid)
        throw new Error("The thread remote did not return a branch head");
    await git.writeRef({ fs, dir: directory, ref: `refs/heads/${thread.branch}`, value: oid, force: true });
    await git.checkout({ fs, dir: directory, ref: thread.branch, force: true });
}
async function configureRemote(fs, thread) {
    if (await git.getConfig({ fs, dir: directory, path: configuredRemotePath }) === thread.remoteUrl)
        return;
    await git.addRemote({ fs, dir: directory, remote: "origin", url: thread.remoteUrl, force: true });
    await git.setConfig({ fs, dir: directory, path: `branch.${thread.branch}.remote`, value: "origin" });
    await git.setConfig({
        fs,
        dir: directory,
        path: `branch.${thread.branch}.merge`,
        value: `refs/heads/${thread.branch}`,
    });
    await git.setConfig({ fs, dir: directory, path: "user.name", value: author.name });
    await git.setConfig({ fs, dir: directory, path: "user.email", value: author.email });
    await git.setConfig({ fs, dir: directory, path: configuredRemotePath, value: thread.remoteUrl });
}
async function status(fs, thread) {
    const matrix = await git.statusMatrix({ fs, dir: directory });
    return {
        branch: thread.branch,
        head: await resolveHead(fs),
        changes: matrix
            .filter(([, headStatus, workdirStatus, stageStatus]) => headStatus !== workdirStatus || headStatus !== stageStatus)
            .map(([filepath]) => filepath),
        remoteUrl: thread.remoteUrl,
    };
}
async function remoteRefs(thread) {
    return git.listServerRefs({ http, url: thread.remoteUrl, protocolVersion: 2, prefix: "refs/heads/" });
}
async function resolveHead(fs) {
    return git.resolveRef({ fs, dir: directory, ref: "HEAD" }).catch(() => undefined);
}
async function exists(fs, path) {
    return fs.promises.stat(path).then(() => true, () => false);
}
export async function withThreadGitLock(thread, operation, signal) {
    if (!navigator.locks) {
        throw new Error("This browser must support Web Locks to safely share the OPFS Git repository");
    }
    const name = `nanocodex-git-${thread.id}`;
    return signal
        ? navigator.locks.request(name, { signal }, operation)
        : navigator.locks.request(name, operation);
}
