/** Bridge a Better Auth cookie-backed session into Nanocodex Connect. */
export function createBetterAuthSessionRoute({ auth, sessions }) {
  if (!auth?.api || typeof auth.api.getSession !== "function") {
    throw new TypeError("Better Auth example requires an auth server instance");
  }
  return sessions.handler({
    async authenticate(request) {
      const session = await auth.api.getSession({ headers: request.headers });
      if (!session?.user?.id) return undefined;
      return { subject: session.user.id };
    },
  });
}
