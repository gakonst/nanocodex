import {
  lazy,
  memo,
  Suspense,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  Activity,
  ArrowDown,
  ArrowUpRight,
  Check,
  ChevronDown,
  ChevronRight,
  CircleHelp,
  Cloud,
  Copy,
  Cpu,
  Folder,
  Globe2,
  HardDrive,
  Layers,
  LoaderCircle,
  Monitor,
  Plus,
  Settings2,
  Square,
  Unplug,
  X,
  Zap,
} from "lucide-react";
import type { DesktopState, Hand, HandConfig, Layout } from "../shared/types";
import { discoveredHands, type Entry } from "./timeline";
import { SignIn } from "./SignIn";
const bridge = window.nanocodex;
const Markdown = lazy(() => import("./Markdown"));
function Streamdown({ children, mode }: { children: string; mode?: string }) {
  return (
    <Suspense fallback={<div className="markdown-fallback">{children}</div>}>
      <Markdown text={children} streaming={mode === "streaming"} />
    </Suspense>
  );
}
export const Message = memo(
  MessageView,
  (a, b) =>
    Object.keys(a.entry).length === Object.keys(b.entry).length &&
    Object.entries(a.entry).every(
      ([key, value]) => b.entry[key as keyof Entry] === value
    )
);
function MessageView({ entry }: { entry: Entry }) {
  const [copied, setCopied] = useState(false);
  if (entry.kind === "tool")
    return (
      <details className="tool-activity">
        <summary>
          {entry.status === "running" ? (
            <LoaderCircle size={14} className="spin" />
          ) : entry.status === "completed" ? (
            <Check size={14} />
          ) : (
            <X size={14} />
          )}
          <span>{toolLabel(entry.name)}</span>
          <span className="tool-preview">{toolPreview(entry)}</span>
          <ChevronRight size={12} />
        </summary>
        <div className="tool-body">
          <pre>{entry.text}</pre>
          {entry.output && (
            <>
              <div className="tool-result-label">Result</div>
              <pre>{entry.output}</pre>
            </>
          )}
        </div>
      </details>
    );
  if (entry.kind === "reasoning")
    return (
      <details className="reasoning">
        <summary>
          {entry.streaming ? "Thinking…" : "Thought process"}
          <ChevronDown size={12} />
        </summary>
        <Streamdown>{entry.text}</Streamdown>
      </details>
    );
  if (entry.kind === "error")
    return (
      <div className="inline-error" role="status">
        {entry.text}
      </div>
    );
  if (entry.kind === "status")
    return (
      <div className="direction-update" role="status">
        <Check size={13} />
        {entry.text}
      </div>
    );
  if (entry.kind === "user")
    return (
      <div className="message user-message">
        <p>{displayPrompt(entry.text)}</p>
        {entry.text.includes("[Execution context:") && (
          <small>
            <Monitor size={11} />
            Selected Hand
          </small>
        )}
      </div>
    );
  return (
    <div className="message assistant-message">
      {entry.agent && (
        <div className="subagent-label">
          <Layers size={12} />
          Agent {entry.agent}
        </div>
      )}
      <Streamdown mode={entry.streaming ? "streaming" : "static"}>
        {entry.text}
      </Streamdown>
      {!entry.streaming && (
        <button
          className="copy-message"
          aria-label="Copy response"
          onClick={() => {
            void navigator.clipboard.writeText(entry.text).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            });
          }}
        >
          {copied ? <Check size={13} /> : <Copy size={13} />}
        </button>
      )}
    </div>
  );
}

export function HandsPage({
  hands,
  remoteHands,
  connected,
  onAdd,
  onEdit,
  onRemote,
  onStart,
  onStop,
  onRemove,
  onDiscover,
  onCloud,
  onUse,
}: {
  hands: Hand[];
  remoteHands: ReturnType<typeof discoveredHands>;
  connected: boolean;
  onAdd(): void;
  onEdit(hand: Hand): void;
  onRemote(): void;
  onStart(id: string): void;
  onStop(id: string): void;
  onRemove(id: string): void;
  onDiscover(): void;
  onCloud(): void;
  onUse(id: string): void;
}) {
  return (
    <div className="settings-scroll">
      <div className="hands-page">
        <div className="page-intro">
          <span className="eyebrow">COMPUTE</span>
          <h1>Many Hands. One workspace.</h1>
          <p>
            Give your agents somewhere to work. Connect this computer, run a
            retained VM, or bring compute from the cloud.
          </p>
          <div className="intro-actions">
            <button
              className="primary-button"
              onClick={onAdd}
              disabled={!connected}
            >
              <Plus size={15} />
              Use this computer
            </button>
            <button className="secondary-button" onClick={onRemote}>
              <Globe2 size={15} />
              Connect another machine
            </button>
          </div>
        </div>
        <div className="compute-overview">
          <div>
            <span className="overview-icon">
              <Cloud size={20} />
            </span>
            <div>
              <strong>Managed brain</strong>
              <small>Your threads and agent state live in the cloud</small>
            </div>
            <span className={`status-badge ${connected ? "online" : ""}`}>
              <i />
              {connected ? "Connected" : "Not connected"}
            </span>
          </div>
          <div className="compute-connection">
            <span />
            <ArrowDown size={14} />
            <span />
          </div>
          <div>
            <span className="overview-icon">
              <Cpu size={20} />
            </span>
            <div>
              <strong>
                {hands.filter((hand) => hand.status === "connected").length}{" "}
                local Hands connected
              </strong>
              <small>Files and commands stay on the Hand you choose</small>
            </div>
            <span className="muted-text">Shared with your agents</span>
          </div>
        </div>
        <div className="subsection-heading">
          <h2>Your compute</h2>
          <span>Stopped Hands contribute no compute</span>
        </div>
        {!hands.length && (
          <div className="empty-hands">
            <Monitor size={28} />
            <h3>Bring your first Hand online</h3>
            <p>
              Choose a local folder to run commands on this computer, or attach
              an isolated Linux VM.
            </p>
            <button
              className="secondary-button"
              onClick={onAdd}
              disabled={!connected}
            >
              <Plus size={14} />
              Use this computer
            </button>
          </div>
        )}
        <div className="hand-grid">
          {hands.map((hand) => (
            <article className="hand-card" key={hand.id}>
              <div className="hand-card-top">
                <span className="hand-icon">
                  {hand.kind === "vm" ? (
                    <HardDrive size={22} />
                  ) : (
                    <Monitor size={22} />
                  )}
                </span>
                <span
                  className={`status-badge ${
                    hand.status === "connected"
                      ? "online"
                      : hand.status === "error"
                      ? "failed"
                      : ""
                  }`}
                >
                  <i />
                  {capitalize(hand.status)}
                </span>
              </div>
              <h3>{hand.name}</h3>
              <p className="hand-workspace">{hand.workspace}</p>
              <div className="hand-facts">
                <span>
                  {hand.kind === "vm"
                    ? `${hand.cpus} CPUs · ${hand.memoryMiB} MiB`
                    : "Runs on this computer"}
                </span>
                <span>
                  {hand.agentId
                    ? "Selected thread only"
                    : "All agents in your account"}
                </span>
              </div>
              <div className="hand-stats">
                <span>
                  <Activity size={13} />
                  {hand.activeCalls} active
                </span>
                <span>{hand.calls} commands</span>
              </div>
              {hand.error && (
                <p className="hand-error" role="alert">
                  {hand.error}
                </p>
              )}
              <div className="hand-actions">
                {hand.status === "connected" || hand.status === "connecting" ? (
                  <button
                    className="secondary-button"
                    onClick={() => onStop(hand.id)}
                  >
                    <Square size={12} />
                    Stop Hand
                  </button>
                ) : (
                  <button
                    className="primary-button"
                    onClick={() => onStart(hand.id)}
                    disabled={!connected}
                  >
                    <Zap size={13} />
                    Start Hand
                  </button>
                )}
                {hand.status === "connected" && (
                  <button
                    className="text-button"
                    onClick={() => onUse(hand.id)}
                  >
                    Use in thread
                    <ArrowUpRight size={13} />
                  </button>
                )}
                {hand.status !== "connected" &&
                  hand.status !== "connecting" && (
                    <button
                      className="icon-button"
                      aria-label={`Edit ${hand.name}`}
                      onClick={() => onEdit(hand)}
                    >
                      <Settings2 size={15} />
                    </button>
                  )}
              </div>
              <details className="hand-log">
                <summary>
                  Activity
                  <ChevronDown size={12} />
                </summary>
                <pre>{hand.logs.join("\n") || "No activity yet."}</pre>
                {hand.status === "stopped" && (
                  <button
                    className="text-button danger"
                    onClick={() => onRemove(hand.id)}
                  >
                    Remove Hand
                  </button>
                )}
              </details>
            </article>
          ))}
        </div>
        <div className="subsection-heading">
          <h2>Cloud & remote Hands</h2>
          <button
            className="text-button"
            disabled={!connected}
            onClick={onDiscover}
          >
            Find connected Hands
            <ArrowUpRight size={13} />
          </button>
        </div>
        {remoteHands.length > 0 && (
          <div className="remote-inventory">
            {remoteHands.map((hand) => (
              <button key={hand.id} onClick={() => onUse(hand.id)}>
                <Cloud size={17} />
                <span>
                  <strong>{hand.name}</strong>
                  <small>{hand.mount}</small>
                </span>
                <ArrowUpRight size={15} />
              </button>
            ))}
            <p>
              Last reported by accountInfo in this thread. Discover again to
              refresh.
            </p>
          </div>
        )}
        <div className="cloud-card">
          <Cloud size={26} />
          <div>
            <h3>Cloudflare compute</h3>
            <p>
              Ask your managed agent to provision a retained Linux workspace. It
              will mount the Hand and report its exact path in the thread.
            </p>
          </div>
          <button
            className="secondary-button"
            onClick={onCloud}
            disabled={!connected}
          >
            Create cloud Hand
            <ArrowUpRight size={13} />
          </button>
        </div>
        <p className="compute-note">
          <CircleHelp size={15} />
          Local Hands run with your OS user’s permissions. Use a VM when you
          need isolation. Keep the app running to provide compute; quitting
          stops its Hands.
        </p>
      </div>
    </div>
  );
}

export function SettingsPage({
  state,
  layout,
  onLayout,
  onState,
  report,
}: {
  state: DesktopState;
  layout: Layout;
  onLayout(layout: Layout): void;
  onState(state: DesktopState): void;
  report(error: unknown): void;
}) {
  const [signingIn, setSigningIn] = useState(false);
  const [busy, setBusy] = useState(false);
  return (
    <div className="settings-scroll">
      <div className="settings-page">
        <div className="page-intro">
          <h1>Settings</h1>
          <p>Make yourself at home.</p>
        </div>
        <section className="settings-section">
          <h2>Appearance</h2>
          <div className="settings-row">
            <span>Tabs</span>
            <div className="segmented-control">
              {(
                [
                  ["left", "Sidebar"],
                  ["top", "Top"],
                ] as const
              ).map(([value, label]) => (
                <button
                  key={value}
                  className={layout.tabPosition === value ? "active" : ""}
                  onClick={() => onLayout({ ...layout, tabPosition: value })}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>
          <div className="settings-row">
            <span>Theme</span>
            <div className="segmented-control">
              {(["system", "light", "dark"] as const).map((value) => (
                <button
                  key={value}
                  className={layout.theme === value ? "active" : ""}
                  onClick={() => onLayout({ ...layout, theme: value })}
                >
                  {capitalize(value)}
                </button>
              ))}
            </div>
          </div>
        </section>
        <section className="settings-section">
          <h2>Account</h2>
          <div className="account-status">
            <span className="avatar">N</span>
            <div>
              <strong>
                {state.connected
                  ? "Connected to Nanocodex"
                  : "Connect your account"}
              </strong>
              <p>
                Your conversations and model connections stay with your account.
              </p>
            </div>
            <span className={`status-badge ${state.connected ? "online" : ""}`}>
              <i />
              {state.connected ? "Connected" : "Offline"}
            </span>
          </div>
          {signingIn ? (
            <SignIn
              baseUrl={state.baseUrl}
              onSignedIn={(value) => {
                setSigningIn(false);
                onState(value);
              }}
              onCancel={() => setSigningIn(false)}
            />
          ) : (
            <button
              className="secondary-button"
              onClick={() => setSigningIn(true)}
            >
              {state.connected ? "Switch account" : "Sign in with your phone"}
            </button>
          )}
          {state.connected && (
            <button
              className="text-button"
              disabled={busy}
              onClick={() => {
                setBusy(true);
                void bridge
                  .disconnect()
                  .then(onState)
                  .catch(report)
                  .finally(() => setBusy(false));
              }}
            >
              <Unplug size={14} />
              Disconnect account
            </button>
          )}
        </section>
        <section className="settings-section">
          <h2>Models & connections</h2>
          <p className="field-help">
            Connect ChatGPT, API providers, apps, MCP servers, or SSH machines.
          </p>
          <button
            className="secondary-button"
            onClick={() => void bridge.openAccount().catch(report)}
          >
            Open account settings
            <ArrowUpRight size={14} />
          </button>
        </section>
        <section className="settings-section">
          <h2>Nanocodex</h2>
          <div className="settings-row">
            <span>Version</span>
            <span>{state.version}</span>
          </div>
          {[
            ["New tab", "⌘ T"],
            ["Close tab", "⌘ W"],
            ["Reopen closed tab", "⇧ ⌘ T"],
            ["Switch tab", "⌘ 1–9"],
            ["Search threads", "⌘ K"],
            ["New line", "⇧ Return"],
          ].map(([label, shortcut]) => (
            <div className="settings-row" key={label}>
              <span>{label}</span>
              <kbd>{shortcut}</kbd>
            </div>
          ))}
        </section>
      </div>
    </div>
  );
}

export function HandDialog({
  initial,
  defaults,
  selectedId,
  onClose,
  onSave,
}: {
  initial: Partial<HandConfig>;
  defaults: Partial<HandConfig>;
  selectedId?: string;
  onClose(): void;
  onSave(config: HandConfig): Promise<void>;
}) {
  const [config, setConfig] = useState<HandConfig>({
    id: `desktop-${crypto.randomUUID().slice(0, 8)}`,
    name: "This computer",
    kind: "local",
    workspace: "",
    cpus: 2,
    memoryMiB: 2048,
    network: true,
    ...defaults,
    ...initial,
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const update = (patch: Partial<HandConfig>) =>
    setConfig((value) => ({ ...value, ...patch }));
  const browse = async (
    field: "workspace" | "binary" | "rootfs" | "guestRuntime",
    kind: "directory" | "file"
  ) => {
    try {
      const path = await bridge.choosePath(kind);
      if (path) update({ [field]: path });
    } catch (cause) {
      setError(String(cause));
    }
  };
  const vmReady = !!config.binary && !!config.rootfs && !!config.guestRuntime;
  return (
    <Modal
      title={initial.id ? "Configure Hand" : "Give your agent a place to work"}
      onClose={onClose}
    >
      <form
        className="hand-form"
        onSubmit={(event) => {
          event.preventDefault();
          setBusy(true);
          setError("");
          void onSave(config)
            .catch((cause) => setError(cause.message))
            .finally(() => setBusy(false));
        }}
      >
        <div className="hand-kind-tabs">
          <button
            type="button"
            className={config.kind === "local" ? "active" : ""}
            onClick={() =>
              update({
                kind: "local",
                name: defaults.name || "This computer",
                workspace: defaults.workspace || "",
              })
            }
          >
            <Monitor size={18} />
            <strong>This computer</strong>
            <span>Ready to use</span>
          </button>
          <button
            type="button"
            className={config.kind === "vm" ? "active" : ""}
            onClick={() =>
              update({
                kind: "vm",
                name: "Linux VM",
                workspace: "/app",
                agentId: undefined,
              })
            }
          >
            <HardDrive size={18} />
            <strong>Linux VM</strong>
            <span>Isolated compute</span>
          </button>
        </div>
        {config.kind === "local" && (
          <>
            <label>
              Workspace folder
              <div className="input-with-button">
                <input
                  aria-label="Workspace folder"
                  required
                  value={config.workspace}
                  onChange={(e) => update({ workspace: e.target.value })}
                />
                <button
                  type="button"
                  onClick={() => void browse("workspace", "directory")}
                >
                  <Folder size={16} />
                  Choose
                </button>
              </div>
            </label>
            <p className="field-help">
              Your agents can read files and run commands on this computer with
              your permissions. The workspace is created for you if you keep the
              default.
            </p>
          </>
        )}
        {config.kind === "vm" && !vmReady && (
          <p className="field-help">
            Choose your prepared Linux image and guest runtime below. Nanocodex
            remembers them for next time.
          </p>
        )}
        <details
          className="advanced-settings"
          open={config.kind === "vm" && !vmReady}
        >
          <summary>
            {config.kind === "vm" && !vmReady
              ? "Set up this VM"
              : "Advanced settings"}
          </summary>
          <label>
            Name
            <input
              aria-label="Hand name"
              value={config.name}
              required
              onChange={(e) => update({ name: e.target.value })}
            />
          </label>
          <label>
            Machine ID
            <input
              aria-label="Machine ID"
              value={config.id}
              required
              onChange={(e) => update({ id: e.target.value })}
            />
          </label>
          {config.kind === "vm" ? (
            <>
              {(
                [
                  ["binary", "nanocodex2 executable"],
                  ["rootfs", "Writable root image (.ext4)"],
                  ["guestRuntime", "Linux guest runtime"],
                ] as const
              ).map(([key, label]) => (
                <label key={key}>
                  {label}
                  <div className="input-with-button">
                    <input
                      aria-label={label}
                      value={config[key] ?? ""}
                      required
                      onChange={(e) => update({ [key]: e.target.value })}
                    />
                    <button
                      type="button"
                      onClick={() => void browse(key, "file")}
                    >
                      <Folder size={15} />
                      Choose
                    </button>
                  </div>
                </label>
              ))}
              <div className="form-grid">
                <label>
                  CPUs
                  <input
                    aria-label="CPUs"
                    type="number"
                    min={1}
                    max={255}
                    value={config.cpus}
                    onChange={(e) => update({ cpus: Number(e.target.value) })}
                  />
                </label>
                <label>
                  Memory (MiB)
                  <input
                    aria-label="Memory (MiB)"
                    type="number"
                    min={128}
                    value={config.memoryMiB}
                    onChange={(e) =>
                      update({ memoryMiB: Number(e.target.value) })
                    }
                  />
                </label>
              </div>
              <label className="check-label">
                <input
                  type="checkbox"
                  checked={config.network}
                  onChange={(e) => update({ network: e.target.checked })}
                />
                Allow internet access
              </label>
            </>
          ) : (
            <label>
              Available to
              <select
                aria-label="Hand scope"
                value={config.agentId ?? ""}
                onChange={(e) =>
                  update({ agentId: e.target.value || undefined })
                }
              >
                <option value="">All agents in my account</option>
                {selectedId && (
                  <option value={selectedId}>Only this thread</option>
                )}
              </select>
            </label>
          )}
        </details>
        {config.kind === "vm" && (
          <p className="field-help">
            Your VM keeps its files across restarts. Stopping the Hand shuts it
            down safely.
          </p>
        )}
        {error && (
          <p className="hand-error" role="alert">
            {error}
          </p>
        )}
        <div className="modal-actions">
          <button
            className="secondary-button"
            type="button"
            disabled={busy}
            onClick={onClose}
          >
            Cancel
          </button>
          <button
            className="primary-button"
            disabled={busy || (config.kind === "vm" && !vmReady)}
          >
            {busy && <LoaderCircle className="spin" size={14} />}Start Hand
          </button>
        </div>
      </form>
    </Modal>
  );
}

export function RemoteDialog({
  origin,
  onClose,
}: {
  origin: string;
  onClose(): void;
}) {
  const [copied, setCopied] = useState(false);
  const command = [
    `export NANOCODEX_MANAGED_URL='${origin}'`,
    "# Set NANOCODEX_API_KEY on the remote machine.",
    "nanocodex2 hand \\",
    "  --vm /path/to/root.ext4 \\",
    "  --vm-guest-runtime /path/to/nanocodex-vm-guest \\",
    "  --vm-workspace /workspace \\",
    "  --vm-cpus 4 --vm-memory-mib 4096 \\",
    "  --machine-id remote-build --machine-name 'Remote build'",
  ].join("\n");
  return (
    <Modal title="Connect another machine" onClose={onClose}>
      <div className="remote-dialog">
        <Globe2 size={30} />
        <p>
          Use another computer’s files and compute from any of your threads.
        </p>
        <ol className="setup-steps">
          <li>Open Nanocodex on the other computer.</li>
          <li>Connect the same Nanocodex account in Settings.</li>
          <li>
            Open Hands, choose <strong>Use this computer</strong>, and start the
            Hand.
          </li>
        </ol>
        <p className="field-help">
          Keep Nanocodex running there. Back here, choose “Find connected Hands”
          to find the connected computer.
        </p>
        <details className="advanced-settings">
          <summary>Advanced: connect a server with the CLI</summary>
          <p className="field-help">
            Start a VM Hand over SSH. It connects outbound to your account
            without an inbound port.
          </p>
          <pre>{command}</pre>
          <button
            className="secondary-button"
            onClick={() =>
              void navigator.clipboard
                .writeText(command)
                .then(() => setCopied(true))
            }
          >
            {copied ? <Check size={14} /> : <Copy size={14} />}Copy command
          </button>
          <p className="field-help">
            Use your prepared image and guest runtime paths. Stop the remote
            Hand with Ctrl-C.
          </p>
        </details>
      </div>
    </Modal>
  );
}

export function Modal({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose(): void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current!;
    dialog.showModal();
    return () => dialog.close();
  }, []);
  return (
    <dialog
      ref={ref}
      className="modal"
      onCancel={onClose}
      onClick={(e) => {
        if (e.target === e.currentTarget) {
          const rect = e.currentTarget.getBoundingClientRect();
          if (
            e.clientX < rect.left ||
            e.clientX > rect.right ||
            e.clientY < rect.top ||
            e.clientY > rect.bottom
          )
            onClose();
        }
      }}
    >
      <div className="modal-heading">
        <h2>{title}</h2>
        <button
          className="icon-button"
          onClick={onClose}
          aria-label="Close dialog"
        >
          <X size={18} />
        </button>
      </div>
      {children}
    </dialog>
  );
}
function capitalize(value: string) {
  return value.charAt(0).toUpperCase() + value.slice(1);
}
export function displayPrompt(value: string) {
  return value.split(
    /\n\n?\[(?:Execution context:|Selected project folder:|Workspace folder:)/
  )[0];
}
function relativeTime(value: number) {
  if (!value) return "";
  const minutes = Math.max(1, Math.floor((Date.now() - value) / 60000));
  return minutes < 60
    ? `${minutes}m`
    : minutes < 1440
    ? `${Math.floor(minutes / 60)}h`
    : `${Math.floor(minutes / 1440)}d`;
}
function toolLabel(name = "Tool") {
  return (
    (
      {
        exec_command: "Ran command",
        exec: "Ran code",
        write_stdin: "Process output",
        accountInfo: "Account & Hands",
        mount: "Mount compute",
        update_plan: "Updated plan",
        code: "Ran code",
      } as Record<string, string>
    )[name] ?? name.replaceAll("_", " ")
  );
}
function toolPreview(entry: Entry) {
  try {
    const value = JSON.parse(entry.text);
    return value.cmd || value.code?.split("\n")[0] || value.query || "";
  } catch {
    return "";
  }
}
