import { useAccountSession } from "./AccountSession";
import { useAccountQuery } from "./useAccountQuery";
import { decodeAdminDetails } from "./adminDetails";

export function AdminPanel({ inline }: { inline: boolean }) {
  const { account } = useAccountSession();
  const { query, refresh } = useAccountQuery(account?.id, "/v1/account/admin", decodeAdminDetails);
  // The server alone decides visibility; never infer access from account identity or organization role.
  if (!query.data || query.isError) return null;
  const { email, phone } = query.data;
  return <details open className={inline ? "wizard-section account-communication" : "account-communication"}>
    <summary>Admin panel</summary>
    <p>Read-only status of your assigned communication services.</p>
    <dl>
      <div><dt>Email</dt><dd>{email.address ?? (!email.assigned ? "Not assigned" : !email.configured ? "Not configured" : "Unavailable")}</dd></div>
      <div><dt>Phone number</dt><dd>{phone.address ?? (!phone.assigned ? "Not assigned" : "Not configured")}</dd></div>
    </dl>
    <button type="button" disabled={query.isFetching} onClick={() => void refresh()}>Refresh services</button>
  </details>;
}
