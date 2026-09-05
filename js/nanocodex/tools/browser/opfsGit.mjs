const WORKSPACE_DIRECTORY = "nanocodex-workspaces";
export async function openOpfsWorkspaceRoot(workspaceName) {
    if (!navigator.storage?.getDirectory) {
        throw new Error("Origin Private File System storage is unavailable in this browser");
    }
    const origin = await navigator.storage.getDirectory();
    const workspaces = await origin.getDirectoryHandle(WORKSPACE_DIRECTORY, { create: true });
    return workspaces.getDirectoryHandle(encodeURIComponent(workspaceName), { create: true });
}
export async function openOpfsGitFs(workspaceName) {
    return createOpfsGitFs(await openOpfsWorkspaceRoot(workspaceName));
}
export function createOpfsGitFs(root) {
    return { promises: createPromises(root) };
}
function createPromises(root) {
    return {
        async readFile(path, options) {
            const relative = normalize(path);
            if (!relative)
                throw fsError("EISDIR", "cannot read a directory");
            try {
                const { parent, name } = await parentHandle(root, relative, false);
                const file = await (await parent.getFileHandle(name)).getFile();
                const maxBytes = typeof options === "object" ? options.maxBytes : undefined;
                const source = maxBytes === undefined ? file : file.slice(0, maxBytes);
                let bytes = new Uint8Array(await source.arrayBuffer());
                if (maxBytes !== undefined && bytes.byteLength > maxBytes) {
                    bytes = bytes.subarray(0, maxBytes);
                }
                const encoding = typeof options === "string" ? options : options?.encoding;
                return encoding ? new TextDecoder(encoding).decode(bytes) : bytes;
            }
            catch (error) {
                throw translateError(error, "ENOENT", `cannot read ${relative}`);
            }
        },
        async writeFile(path, value) {
            const relative = normalize(path);
            if (!relative)
                throw fsError("EISDIR", "cannot write a directory");
            try {
                const { parent, name } = await parentHandle(root, relative, false);
                const handle = await parent.getFileHandle(name, { create: true });
                const writable = await handle.createWritable();
                try {
                    await writable.write(asWriteValue(value));
                    await writable.close();
                }
                catch (error) {
                    await writable.abort(error).catch(() => undefined);
                    throw error;
                }
            }
            catch (error) {
                throw translateError(error, "ENOENT", `cannot write ${relative}`);
            }
        },
        async appendFile(path, value) {
            const relative = normalize(path);
            if (!relative)
                throw fsError("EISDIR", "cannot append to a directory");
            try {
                const { parent, name } = await parentHandle(root, relative, false);
                const handle = await parent.getFileHandle(name, { create: true });
                const size = (await handle.getFile()).size;
                const writable = await handle.createWritable({ keepExistingData: true });
                try {
                    await writable.seek(size);
                    await writable.write(asWriteValue(value));
                    await writable.close();
                }
                catch (error) {
                    await writable.abort(error).catch(() => undefined);
                    throw error;
                }
            }
            catch (error) {
                throw translateError(error, "ENOENT", `cannot append ${relative}`);
            }
        },
        async unlink(path) {
            await remove(root, normalize(path), false);
        },
        async readdir(path) {
            const directory = await directoryHandle(root, normalize(path), false);
            const names = [];
            const entries = directoryEntries(directory);
            for await (const [name] of entries)
                names.push(name);
            return names;
        },
        async readdirWithFileTypes(path) {
            const directory = await directoryHandle(root, normalize(path), false);
            const entries = [];
            for await (const [name, handle] of directoryEntries(directory)) {
                entries.push({
                    name,
                    isFile: handle.kind === "file",
                    isDirectory: handle.kind === "directory",
                    isSymbolicLink: false,
                });
            }
            return entries;
        },
        async mkdir(path) {
            await directoryHandle(root, normalize(path), true);
        },
        async rmdir(path) {
            const relative = normalize(path);
            if (!relative)
                throw fsError("EPERM", "cannot remove workspace root");
            await remove(root, relative, false);
        },
        async rm(path, options) {
            const relative = normalize(path);
            if (!relative)
                throw fsError("EPERM", "cannot remove workspace root");
            await remove(root, relative, Boolean(options?.recursive));
        },
        async stat(path) {
            return stat(root, normalize(path));
        },
        async lstat(path) {
            return stat(root, normalize(path));
        },
        async readlink() {
            throw fsError("ENOSYS", "OPFS does not support symbolic links");
        },
        async symlink() {
            throw fsError("ENOSYS", "OPFS does not support symbolic links");
        },
    };
}
function directoryEntries(directory) {
    return directory.entries();
}
async function stat(root, relative) {
    try {
        const handle = relative ? await entryHandle(root, relative) : root;
        if (handle.kind === "directory") {
            return fileStat("directory", 0, 0);
        }
        const file = await handle.getFile();
        return fileStat("file", file.size, file.lastModified);
    }
    catch (error) {
        throw translateError(error, "ENOENT", `cannot stat ${relative}`);
    }
}
function fileStat(kind, size, modifiedAt) {
    return {
        size,
        mode: kind === "directory" ? 0o040755 : 0o100644,
        mtimeMs: modifiedAt,
        ctimeMs: modifiedAt,
        isFile: () => kind === "file",
        isDirectory: () => kind === "directory",
        isSymbolicLink: () => false,
    };
}
async function entryHandle(root, relative) {
    const { parent, name } = await parentHandle(root, relative, false);
    try {
        return await parent.getFileHandle(name);
    }
    catch (fileError) {
        try {
            return await parent.getDirectoryHandle(name);
        }
        catch {
            throw fileError;
        }
    }
}
async function directoryHandle(root, relative, create) {
    let directory = root;
    if (!relative)
        return directory;
    try {
        for (const segment of relative.split("/")) {
            directory = await directory.getDirectoryHandle(segment, { create });
        }
        return directory;
    }
    catch (error) {
        throw translateError(error, "ENOENT", `cannot open directory ${relative}`);
    }
}
async function parentHandle(root, relative, create) {
    const segments = relative.split("/");
    const name = segments.pop();
    if (!name)
        throw fsError("EINVAL", "path cannot be empty");
    return { parent: await directoryHandle(root, segments.join("/"), create), name };
}
async function remove(root, relative, recursive) {
    try {
        const { parent, name } = await parentHandle(root, relative, false);
        await parent.removeEntry(name, { recursive });
    }
    catch (error) {
        throw translateError(error, "ENOENT", `cannot remove ${relative}`);
    }
}
function normalize(path) {
    if (typeof path !== "string")
        throw fsError("EINVAL", "path must be a string");
    const raw = path.replace(/\\/g, "/").replace(/^\/+/, "");
    const relative = raw === "workspace" ? "" : raw.startsWith("workspace/") ? raw.slice(10) : raw;
    const segments = [];
    for (const segment of relative.split("/")) {
        if (!segment || segment === ".")
            continue;
        if (segment === "..")
            throw fsError("EPERM", "path escapes the workspace");
        segments.push(segment);
    }
    return segments.join("/");
}
function asWriteValue(value) {
    if (typeof value === "string" || value instanceof Blob)
        return value;
    if (value instanceof ArrayBuffer)
        return value;
    if (ArrayBuffer.isView(value)) {
        const bytes = new Uint8Array(value.byteLength);
        bytes.set(new Uint8Array(value.buffer, value.byteOffset, value.byteLength));
        return bytes;
    }
    throw fsError("EINVAL", "file contents must be bytes or text");
}
function translateError(error, fallback, message) {
    if (error && typeof error === "object" && "code" in error &&
        typeof error.code === "string") {
        return error;
    }
    const name = error instanceof DOMException ? error.name : "";
    const code = name === "NotFoundError" ? "ENOENT"
        : name === "TypeMismatchError" ? "ENOTDIR"
            : name === "InvalidModificationError" ? "ENOTEMPTY"
                : name === "NoModificationAllowedError" ? "EPERM"
                    : fallback;
    return fsError(code, message, error);
}
function fsError(code, message, cause) {
    return Object.assign(new Error(message, cause === undefined ? undefined : { cause }), { code });
}
