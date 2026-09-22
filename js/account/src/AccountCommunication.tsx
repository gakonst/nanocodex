import { useAccountSession } from "./AccountSession";
import { useAccountQuery } from "./useAccountQuery";
import { decodeAccountCommunication } from "./agentContactDetails";
import "./AccountCommunication.css";

export function AccountCommunication({ inline }: { inline: boolean }) {
  const { account } = useAccountSession();
  const { query, refresh } = useAccountQuery(account?.id, "/v1/account/communication", decodeAccountCommunication);
  return <section className={inline ? "wizard-section account-communication" : "account-communication"} aria-labelledby="communication-heading">
    <div className={inline ? "wizard-section-title" : "api-key-heading"}>
      <div><h2 id="communication-heading">Agent contact details</h2><p>The email and phone number assigned to your Nanocodex agents.</p></div>
    </div>
    {query.isPending ? <p role="status">Loading contact details…</p> : query.isError ?
      <div className="account-failure" role="alert"><p>Couldn’t load your agent contact details.</p><button type="button" onClick={() => void refresh()}>Retry</button></div> :
      <dl><div><dt>Email</dt><dd>{query.data?.email ?? "Not assigned"}</dd></div><div><dt>Phone number</dt><dd>{query.data?.phone ?? "Not assigned"}</dd></div></dl>}
  </section>;
}
