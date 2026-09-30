const groups = {
  Browser: ["getVersion", "close"],
  Target: [
    "getTargets",
    "getTargetInfo",
    "createTarget",
    "attachToTarget",
    "detachFromTarget",
    "closeTarget",
  ],
  Page: ["enable", "disable", "navigate", "reload", "getFrameTree"],
  Runtime: [
    "enable",
    "disable",
    "evaluate",
    "callFunctionOn",
    "releaseObject",
    "releaseObjectGroup",
  ],
  DOM: [
    "enable",
    "disable",
    "getDocument",
    "describeNode",
    "querySelector",
    "querySelectorAll",
    "getOuterHTML",
    "getAttributes",
    "resolveNode",
    "focus",
  ],
  Input: ["insertText"],
};
export const protocol = {
  version: { major: "1", minor: "3" },
  domains: Object.entries(groups).map(([domain, names]) => ({
    domain,
    description: `Experimental Obscura ${domain} subset. Unsupported CDP commands return -32601.`,
    commands: names.map((name) => ({ name })),
    events: [],
    types: [],
  })),
};
