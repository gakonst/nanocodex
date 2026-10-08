import { useEffect, useRef, useState } from "react";
import { Link, useNavigate } from "react-router";
import { ArrowUpRight, Bot, Check, Copy, LockKeyhole, Monitor, Moon, Phone, Plug, Sun } from "lucide-react";
import { AccountChooser } from "nanocodex-connect-ui/AccountChooser";
import { ConnectionLogo } from "nanocodex-connect-ui/ConnectionLogo";
import { useAccountSession } from "./AccountSession";
import { pathForSurface } from "./navigation";
import { preloadAgentExperience } from "./routeModulePreloads";
import "./HomeLanding.css";


const installCommand = "curl -fsSL https://nanocodex.paradigm.xyz | bash";
const cliSteps = [
  { label: "Install", command: installCommand },
  { label: "Sign in", command: "nanocodex2 login" },
  { label: "Start working", command: "nanocodex2" },
] as const;
const logos = ["gmail", "gcalendar", "gdrive", "github", "slack", "x", "spotify", "chatgpt", "cloudflare", "mcp"] as const;
const features = [
  { icon: Plug, title: "Connections", body: "Gmail, Calendar, Drive, GitHub, Slack, X and any MCP server. Tokens stay in the broker, never in prompts." },
  { icon: LockKeyhole, title: "Vault", body: "Logins, cards and authenticator codes your agent can use privately, without ever seeing the secret." },
  { icon: Monitor, title: "Your computers", body: "The CLI turns each machine into a Hand, so durable cloud agents can run commands and drive apps on it." },
  { icon: Phone, title: "Phone & wallet", body: "Dedicated SMS numbers, outbound calls and an account wallet for paid tools, all behind your approval." },
] as const;

type Theme = "light" | "dark";
type ThemeProps = { theme?: Theme; onThemeChange?: (theme: Theme) => void };

/**
 * The homepage is the same product page for every visitor. Signed-out
 * visitors get a sign-in form; signed-in accounts get shortcuts to their
 * agents (`/agents`) and account connections (`/account`) instead.
 */
export function HomeLanding({ theme, onThemeChange }: ThemeProps) {
  return <HomeMarketing theme={theme} onThemeChange={onThemeChange} />;
}

function HomeLoading() {
  return <div className="home-landing-loading" role="status"><span className="account-loading-dot" />Checking your account…</div>;
}

function useTheme(controlled: Theme | undefined, onChange: ((theme: Theme) => void) | undefined) {
  const [local, setLocal] = useState<Theme>(() => document.documentElement.dataset.theme === "light" ? "light" : "dark");
  const theme = controlled ?? local;
  const toggle = () => {
    const next: Theme = theme === "light" ? "dark" : "light";
    if (onChange) onChange(next);
    else {
      setLocal(next);
      document.documentElement.dataset.theme = next;
      localStorage.setItem("nanocodex-theme", next);
    }
  };
  return [theme, toggle] as const;
}

function CopyCommand({ command, label }: { command: string; label: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const copy = () => {
    void navigator.clipboard?.writeText(command).then(() => {
      setCopied(true);
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => setCopied(false), 1600);
    }, () => undefined);
  };
  return (
    <div className="home-command">
      <span className="home-command-label">{label}</span>
      <code><span aria-hidden="true">$ </span>{command}</code>
      <button type="button" onClick={copy} aria-label={`Copy ${label.toLowerCase()} command`} title="Copy">
        {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
      </button>
    </div>
  );
}

function HomeMarketing({ theme: controlledTheme, onThemeChange }: ThemeProps) {
  const session = useAccountSession();
  const navigate = useNavigate();
  const account = session.account?.persistent ? session.account : null;
  const checking = session.status === "checking" && !account;
  const [theme, toggleTheme] = useTheme(controlledTheme, onThemeChange);
  const signIn = useRef<HTMLElement>(null);
  const signInRequested = useRef(false);
  const agentsPath = pathForSurface("agent");
  const agentIntent = { onFocus: preloadAgentExperience, onPointerEnter: preloadAgentExperience, onPointerDown: preloadAgentExperience };
  const accountPath = pathForSurface("connect");
  // An explicit sign-in from this page continues to the account connections;
  // an existing session simply sees the signed-in homepage.
  useEffect(() => {
    if (!account || !signInRequested.current) return;
    signInRequested.current = false;
    navigate(accountPath);
  }, [account, accountPath, navigate]);
  const focusSignIn = () => {
    signIn.current?.scrollIntoView({ behavior: "smooth", block: "center" });
    signIn.current?.querySelector<HTMLInputElement>("input")?.focus({ preventScroll: true });
  };
  return (
    <div className="home-landing" data-testid="home-landing">
      <header className="home-landing-topbar">
        <a href="/" className="home-landing-brand" aria-label="Nanocodex home">
          <svg aria-hidden="true" viewBox="76 76 872 872"><rect x="76" y="76" width="872" height="872" rx="194" fill="#292929" /><path d="M326 695V332L638 695V332" fill="none" stroke="#f7f7f7" strokeWidth="67" strokeLinecap="round" strokeLinejoin="round" /><circle cx="742" cy="691" r="27" fill="#8cb38c" /></svg>
          <span>Nanocodex</span>
        </a>
        <nav className="home-landing-links" aria-label="Site">
          <a href="/docs">Docs</a>
          <Link to={agentsPath} {...agentIntent}>Agents</Link>
          <a href="/changelog">Changelog</a>
          <a href="https://github.com/gakonst/nanocodex" target="_blank" rel="noreferrer">GitHub <ArrowUpRight aria-hidden="true" /></a>
          <button className="home-icon-button" type="button" onClick={toggleTheme} aria-label={`Use ${theme === "light" ? "dark" : "light"} appearance`} title="Change appearance">
            {theme === "light" ? <Moon aria-hidden="true" /> : <Sun aria-hidden="true" />}
          </button>
          {account ? (
            <Link className="home-button home-button--small" to={accountPath}>Account</Link>
          ) : checking ? null : (
            <button className="home-button home-button--small" type="button" onClick={focusSignIn}>Sign in</button>
          )}
        </nav>
      </header>
      <main className="home-landing-main">
        <section className="home-hero" aria-labelledby="home-title">
          <p className="home-eyebrow"><Bot aria-hidden="true" /> Durable agents for your accounts and computers</p>
          <h1 id="home-title">Your agent, connected to everything you already use.</h1>
          <p className="home-lede">
            Nanocodex runs frontier coding agents from your terminal and keeps them working in the cloud.
            Sign in once, connect your accounts, and let them act across email, calendar, code and your own machines.
          </p>
          <div className="home-hero-actions">
            {account ? <>
              <Link className="home-button" to={agentsPath} {...agentIntent}>Open your agents</Link>
              <Link className="home-button home-button--ghost" to={accountPath}>Manage connections</Link>
            </> : <>
              <button className="home-button" type="button" onClick={focusSignIn} disabled={checking}>Sign in to connect accounts</button>
              <a className="home-button home-button--ghost" href="#home-cli">Install the CLI</a>
            </>}
          </div>
          <ul className="home-logos" aria-label="Supported connections">
            {logos.map((id) => <li key={id}><ConnectionLogo id={id} /></li>)}
          </ul>
        </section>
        <section className="home-grid" aria-label="Get started">
          <article className="home-panel home-cli" id="home-cli" aria-labelledby="home-cli-title">
            <h2 id="home-cli-title">Start in your terminal</h2>
            <p>Install the CLI on macOS or Linux, sign in with your phone number, and your computer joins your account as a Hand.</p>
            <div className="home-commands">
              {cliSteps.map((step) => <CopyCommand key={step.label} command={step.command} label={step.label} />)}
            </div>
            <p className="home-fine">Windows: <code>irm https://nanocodex.paradigm.xyz/install.ps1 | iex</code></p>
          </article>
          {account ? (
            <section className="home-panel home-signed-in" aria-labelledby="home-signed-in-title">
              <h2 id="home-signed-in-title">You’re signed in</h2>
              <p>{account.address ? `Personal account ${account.address.slice(0, 6)}…${account.address.slice(-4)}` : "Your personal account"} is ready. Pick up a conversation or manage what your agents can reach.</p>
              <div className="home-signed-in-actions">
                <Link className="home-button" to={agentsPath} {...agentIntent}>Agents</Link>
                <Link className="home-button home-button--ghost" to={accountPath}>Account &amp; connections</Link>
              </div>
            </section>
          ) : checking ? (
            <section className="home-panel home-sign-in" aria-label="Account">
              <HomeLoading />
            </section>
          ) : (
          <section className="home-panel home-sign-in connect-onboarding" ref={signIn} aria-labelledby="home-sign-in-title">
            <h2 id="home-sign-in-title">Sign in on the web</h2>
            <p>Use the same account as the CLI. After signing in you’ll land on your Connections.</p>
            <AccountChooser disabled={session.operation !== null} failure={session.error}
              onChooseAccount={(selection) => {
                signInRequested.current = true;
                void session.chooseAccount(selection);
              }} />
          </section>
          )}
        </section>
        <section className="home-features" aria-label="What you can connect">
          {features.map((feature) => (
            <article key={feature.title} className="home-feature">
              <feature.icon aria-hidden="true" />
              <h3>{feature.title}</h3>
              <p>{feature.body}</p>
            </article>
          ))}
        </section>
      </main>
      <footer className="home-landing-footer">
        <span>Built by Paradigm</span>
        <nav aria-label="Footer"><a href="/docs">Docs</a><a href="/code">Source</a><a href="/evals">Evals</a><a href="/router">Router</a></nav>
      </footer>
    </div>
  );
}
