import { PhoneService } from "./PhoneService";
import { useEffect, useRef, useState } from "react";
import { NavLink, useLocation } from "react-router";
import { ArrowUpRight, CircleUserRound, KeyRound, LockKeyhole, LogOut, Moon, Phone, Plug, Sun, Wallet } from "lucide-react";
import { AccountChooser } from "nanocodex-connect-ui/AccountChooser";
import { AccountMenu } from "./AccountMenu";
import { Vault } from "./Vault";
import { useAccountSession } from "./AccountSession";
import "./AccountWorkspace.css";

const sections = [
  { path: "/connect", label: "Connections", icon: Plug, section: "connections" },
  { path: "/connect/vault", label: "Vault", icon: LockKeyhole, section: "vault" },
  { path: "/services/phone", label: "Phone numbers", icon: Phone, section: "phone" },
  { path: "/connect/wallet", label: "Wallet", icon: Wallet, section: "wallet" },
  { path: "/connect/access", label: "API access", icon: KeyRound, section: "access" },
] as const;

export function ConnectHome({ theme: controlledTheme, onThemeChange }: {
  theme?: "light" | "dark";
  onThemeChange?: (theme: "light" | "dark") => void;
}) {
  const location = useLocation();
  const session = useAccountSession();
  const [localTheme, setLocalTheme] = useState<"light" | "dark">(() => document.documentElement.dataset.theme === "light" ? "light" : "dark");
  const theme = controlledTheme ?? localTheme;
  const section = sections.find(item => item.path === (location.pathname.replace(/\/+$/, "") === "/vault" ? "/connect/vault" : location.pathname.replace(/\/+$/, ""))) ?? sections[0];
  const account = session.account?.persistent ? session.account : null;
  const heading = useRef<HTMLHeadingElement>(null);
  const previousPath = useRef(location.pathname);

  useEffect(() => {
    document.title = `${section.label} · Nanocodex`;
    if (previousPath.current !== location.pathname) {
      const target = heading.current ?? document.querySelector<HTMLHeadingElement>("#account-content h1");
      target?.focus({ preventScroll: true });
    }
    previousPath.current = location.pathname;
  }, [location.pathname, section.label]);

  const toggleTheme = () => {
    const next = theme === "light" ? "dark" : "light";
    if (onThemeChange) onThemeChange(next);
    else {
      setLocalTheme(next);
      document.documentElement.dataset.theme = next;
      localStorage.setItem("nanocodex-theme", next);
    }
  };

  return (
    <div className="account-hub" data-testid="connect-home">
      <a className="account-skip" href="#account-content">Skip to content</a>
      <header className="account-hub-topbar">
        <NavLink to="/" className="account-hub-brand" aria-label="Nanocodex home">
          <svg aria-hidden="true" viewBox="76 76 872 872"><rect x="76" y="76" width="872" height="872" rx="194" fill="#292929" /><path d="M326 695V332L638 695V332" fill="none" stroke="#f7f7f7" strokeWidth="67" strokeLinecap="round" strokeLinejoin="round" /><circle cx="742" cy="691" r="27" fill="#8cb38c" /></svg>
          <span>Nanocodex</span>
        </NavLink>
        <div className="account-hub-utilities">
          {account ? <button className="account-icon-button account-mobile-sign-out" type="button" disabled={session.operation !== null} onClick={() => void session.signOut()} aria-label="Sign out" title="Sign out"><LogOut aria-hidden="true" /></button> : null}
          <a href="/docs" className="account-docs">Docs <ArrowUpRight aria-hidden="true" /></a>
          <button className="account-icon-button" type="button" onClick={toggleTheme} aria-label={`Use ${theme === "light" ? "dark" : "light"} appearance`} title="Change appearance">
            {theme === "light" ? <Moon aria-hidden="true" /> : <Sun aria-hidden="true" />}
          </button>
        </div>
      </header>
      {session.status === "checking" ? (
        <div className="account-hub-loading" role="status"><span className="account-loading-dot" />Opening your account…</div>
      ) : !account ? (
        <section id="account-content" tabIndex={-1} className="account-hub-sign-in connect-onboarding" aria-label="Sign in">
          <AccountChooser disabled={session.operation !== null} failure={session.error}
            onChooseAccount={selection => void session.chooseAccount(selection)} />
          <p className="account-sign-in-destination">Continue to {section.label.toLowerCase()}</p>
        </section>
      ) : (
        <div className="account-hub-layout">
          <aside className="account-hub-sidebar">
            <span className="account-nav-label">Your account</span>
            <nav aria-label="Account navigation">
              {sections.map(item => <NavLink end={item.path === "/connect"} key={item.path} to={item.path}>
                <item.icon aria-hidden="true" /><span>{item.label}</span>
              </NavLink>)}
            </nav>
            <div className="account-hub-identity">
              <CircleUserRound aria-hidden="true" />
              <div><strong>Personal account</strong><span>{account.address ? `${account.address.slice(0, 6)}…${account.address.slice(-4)}` : `Account ${account.id.slice(0, 8)}`}</span></div>
              <button className="account-icon-button" type="button" disabled={session.operation !== null} onClick={() => void session.signOut()} aria-label="Sign out" title="Sign out"><LogOut aria-hidden="true" /></button>
            </div>
          </aside>
          <section id="account-content" tabIndex={-1} className={`account-hub-body account-hub-${section.section}`} aria-label={section.label}>
            {section.section !== "vault" ? <header className="account-page-heading"><h1 ref={heading} tabIndex={-1}>{section.label}</h1></header> : null}
            {session.error ? <div className="account-workspace-error" role="alert">{session.error}<button type="button" onClick={() => void session.refresh()}>Try again</button></div> : null}
            {section.section === "phone" ? <PhoneService /> : section.section === "vault" ? <Vault key={account.id} /> : <AccountMenu key={`${account.id}:${section.section}`} inline section={section.section} />}
          </section>
        </div>
      )}
    </div>
  );
}
