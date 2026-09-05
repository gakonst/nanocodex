import { AccountMenu } from "./AccountMenu";
import { NavLink, useLocation } from "react-router";
import { Vault } from "./Vault";

export function ConnectHome() {
  const location = useLocation();
  const vault = location.pathname.replace(/\/+$/, "") === "/connect/vault";
  return (
    <div className="device-connect-route connect-home" data-testid="connect-home">
      <section className="connect-wizard">
        <nav className="connect-home-navigation" aria-label="Connect settings">
          <NavLink end to="/connect">Connect</NavLink>
          <NavLink to="/connect/vault">Vault</NavLink>
        </nav>
        <div className="wizard-content">
          {vault ? <Vault /> : <AccountMenu inline />}
        </div>
      </section>
    </div>
  );
}
