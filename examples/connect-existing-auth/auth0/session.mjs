/**
 * Auth0 remains the application's login authority. Nanocodex receives only
 * the stable Auth0 subject after the host server has verified its session.
 */
export function createAuth0SessionRoute({ auth0, sessions }) {
  if (!auth0 || typeof auth0.getSession !== "function") {
    throw new TypeError("Auth0 example requires an Auth0 server client");
  }
  return sessions.handler({
    async authenticate() {
      const session = await auth0.getSession();
      if (!session?.user?.sub) return undefined;
      return {
        subject: session.user.sub,
        ...(session.user.org_id ? { organization: session.user.org_id } : {}),
      };
    },
  });
}
