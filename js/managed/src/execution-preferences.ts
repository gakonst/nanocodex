/** Shared placement guidance for every managed model backend. */
export const HAND_EXECUTION_INSTRUCTIONS = [
  "For native commands and computer use, choose an execution Hand explicitly. An explicit user target or the Hand holding the relevant workspace, application, or retained process takes precedence. Otherwise prefer the current turn's request_origin.hand when it is online and has the required capabilities. request_origin is an authorized client-reported placement hint, not proof of physical identity or a grant of access. A later turn may come from another device; use its current request_origin, not the thread's original startup snapshot. Keep lightweight cloud scratch work in /brain; an omitted shell workdir still means /brain.",
  "request_origin.native_cwd describes the initial native directory on that origin Hand only. Use an explicit command cwd or a properly quoted cd on the chosen Hand. Never reinterpret native_cwd as a logical workdir, move the Hand mount, or create a per-workspace Hand. It grants no authority and does not apply to another Hand.",
  "If the preferred Hand is unavailable or unsuitable, use environment() to refresh the authorized catalog and choose the best suitable online user Hand before provisioning anything. Use execution_preferences as an advisory starting point, then match the task's OS, workspace, CPU, RAM, free disk, load, and screen capabilities. Missing resources are unknown, never zero. Last observed CPU and total RAM describe hardware size; stale available RAM, free disk, and load do not establish current capacity. Inspect the Hand before relying on stale capacity measurements. An online row is a discovery hint: verify the required shell or CUA capability by using it. Do not cycle through historical aliases of the same unavailable device. Prefer an existing capable Linux or other user Hand for builds and browser work over creating a sandbox.",
  "Before mounting a sandbox when no suitable attached Hand works, inspect configured SSH recovery targets with server_hand list. For a server the user has authorized for this task, try its exact saved target and trusted host key using ssh -o IdentityRef=REFERENCE USER@HOST -- COMMAND, or server_hand connect when a desktop Hand is needed. Saved target metadata must identify that server; never guess that an offline label is an SSH hostname, connect to an unrelated target, or export credentials. SSH shell access alone does not provide a CUA screen. If recovery is unavailable or fails, a sandbox is the final fallback unless the user explicitly requests isolation or a VM.",
  "Fallback selects the destination of a new command only. Never silently reroute an admitted command, retained process session, or CUA action. If a Hand disconnects after submission, reconcile that operation on its original Hand before retrying; unknown outcome may have side effects. Workspaces and browser state on user Hands are not automatically shared. Verify the required files or application state before continuing on another Hand. Never claim user-visible app, window, or desktop state is running or restored until an observation confirms it; otherwise report what was done and what remains unverified.",
].join("\n\n");

/** A headed browser can remain in the background; never imply foreground control. */
export const BACKGROUND_BROWSER_INSTRUCTIONS = "For browser work, prefer the provider's supported background tabs and agent-owned tab groups, reusing the task's existing group and browser session. Discover the exact provider contract and use only its documented tab/group APIs and visibility options. Headed means a real browser, not permission to focus it. Do not activate the user's browser, switch their active tab, open a foreground window, or send global browser shortcuts unless the user explicitly requested interaction with that window. Native app selection or a new browser window does not provide background tab isolation. If the provider reports Browser APIs are disabled, report that limitation and use another suitable Hand with supported background tabs or an isolated desktop; do not silently replace background browsing with native control of the user's active browser. If no non-disruptive supported path exists, leave that browser step blocked. Never claim a tab group or background isolation exists without provider evidence.";

export const HEADED_CUA_INSTRUCTIONS = [
  "For interactive website tasks, use a headed browser through the CUA tool on a suitable online Hand by default. Prefer the submitting Hand when it supports the required non-disruptive browser surface, then another suitable attached user Hand. Discover that Hand's exact CUA contract with its workdir and inspect the intended tab or isolated screen before acting. Retain the same Hand/browser for multi-step workflows. Do not start a separate headless browser, Playwright, Puppeteer, raw CDP, or browser_execute merely because it is convenient. Browser test suites may use their existing automation when testing code is the task; direct service APIs remain appropriate for API tasks.",
  BACKGROUND_BROWSER_INSTRUCTIONS,
  "Hosted browser tools are fallbacks when no authorized Hand can provide working non-disruptive headed CUA, or when a supported private credential flow requires them. Never type or read back passwords, payment credentials, or verification codes through ordinary CUA; use the secure Vault/private-input tools for them. If the user prefers to type secrets into their own visible browser, skip Vault/private input: drive that browser with CUA to the sign-in or secret field, tell the user exactly what to enter, stop until they say done, then continue. A failure on one Hand does not establish that every Hand is unavailable. Check another suitable online Hand and authorized SSH recovery before provisioning a browser sandbox. Respect human verification and private takeover requirements.",
].join("\n\n");

import { projectHandResources, type AccountEnvironmentSource } from "nanocodex/tools/environment";

type Hand = NonNullable<AccountEnvironmentSource["machines"]>[number];
type Origin = Readonly<{ hand?: Readonly<{ key: string; path: string }> | null; cwd?: string | null }>;
const GIB = 1024 ** 3;

/** Advisory only: namespace dispatch and retained process targets never change. */
export function projectExecutionPreferences(hands: readonly Hand[], origin: Origin, now = Date.now()) {
  const candidates = (mode: "native" | "computer") => {
    const eligible = hands.filter(hand => (hand.online === true || hand.kind === "sandbox" && hand.online !== false)
      && (mode === "native" ? hand.capabilities.some(capability => ["process", "shell"].includes(capability))
        : hand.capabilities.includes("computer")));
    const ranked = eligible.map(hand => {
      const resources = projectHandResources(hand.resources, hand.online, now);
      const fresh = resources.status === "fresh" ? resources : undefined;
      const constraints = [
        ...(fresh?.disk_available_bytes !== undefined && fresh.disk_available_bytes < GIB ? ["low_disk"] : []),
        ...(fresh?.memory_available_bytes !== undefined && fresh.memory_available_bytes < 256 * 1024 ** 2 ? ["low_memory"] : []),
      ];
      const originMatch = hand.id === origin.hand?.key;
      const workdir = originMatch && origin.cwd && (origin.cwd === hand.mount || origin.cwd.startsWith(`${hand.mount}/`))
        ? origin.cwd : hand.mount;
      const hardware = resources.status === "unknown" ? undefined : resources;
      return { hand, workdir, originMatch, constraints, resources, fresh, hardware };
    }).sort((a, b) => {
      // Existing user resources take precedence over hosted sandboxes. Within
      // that tier, severe observed pressure makes another Hand more suitable.
      const tier = (hand: Hand) => hand.kind === "sandbox" ? 1 : 0;
      const preference = tier(a.hand) - tier(b.hand)
        || Number(a.constraints.length > 0) - Number(b.constraints.length > 0)
        || Number(b.originMatch) - Number(a.originMatch);
      if (preference) return preference;
      // Known hardware size and fresh free capacity sort ahead of unknowns. Missing values
      // remain explicitly unknown in the output, never measured zeroes.
      const metrics = ["cpu_logical_count", "memory_total_bytes", "memory_available_bytes", "disk_available_bytes"] as const;
      for (const metric of metrics) {
        const staticMetric = metric === "cpu_logical_count" || metric === "memory_total_bytes";
        const av = (staticMetric ? a.hardware : a.fresh)?.[metric];
        const bv = (staticMetric ? b.hardware : b.fresh)?.[metric];
        if (av === undefined && bv !== undefined) return 1;
        if (av !== undefined && bv === undefined) return -1;
        if (av !== undefined && bv !== undefined && av !== bv) return bv - av;
      }
      return a.hand.id.localeCompare(b.hand.id);
    });
    return {
      recommended_workdir: ranked[0]?.workdir ?? null,
      candidates: ranked.map(({ hand, workdir, originMatch, constraints, resources }) => ({
        hand: hand.id, workdir, submitting_hand: originMatch, resources_status: resources.status, constraints,
      })),
    };
  };
  return {
    advisory: true as const,
    origin_hand: origin.hand?.key ?? null,
    native: candidates("native"),
    computer: candidates("computer"),
    browser: "headed_cua" as const,
    fallback_order: ["explicit_task_target", "suitable_submitting_hand", "suitable_online_user_hand", "authorized_ssh_recovery", "sandbox"],
  };
}
