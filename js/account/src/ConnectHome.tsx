import { AccountMenu } from "./AccountMenu";
import { NavLink, useLocation } from "react-router";
import { Vault } from "./Vault";
import { RemoteScreens } from "./RemoteScreens";
import { useAccountSession } from "./AccountSession";

export function ConnectHome() {
  const location = useLocation();
  const { account } = useAccountSession();
  const vault = location.pathname.replace(/\/+$/, "") === "/connect/vault";
  return (
    <div className="device-connect-route connect-home" data-testid="connect-home">
      <section className="connect-wizard">
        <nav className="connect-home-navigation" aria-label="Connect settings">
          <NavLink end to="/connect">Connect</NavLink>
          <NavLink to="/connect/vault">Vault</NavLink>
          {account?.persistent && <RemoteScreens key={account.id} showLabel />}
        </nav>
        <div className="wizard-content">
          {vault ? <Vault key={account?.id ?? "signed-out"} /> : <AccountMenu inline />}
        </div>
      </section>
    </div>
  );
}
