import { Suspense, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router";
import { AccountSessionProvider } from "./AccountSession";
import { NanocodexApp } from "./NanocodexApp";
import { ArtifactRuntime } from "./artifactRuntime";
import {
  prepareRepositorySurface,
  preloadDirectSurface,
  type PreparedDirectRoute,
} from "./routeLoaders";
import { surfaceFromUrl } from "./navigation";

const directUrl = new URL(window.location.href);
const directPath = directUrl.pathname === "/"
  ? "/"
  : directUrl.pathname.replace(/\/+$/, "");
const container = document.getElementById("root");
if (!container) throw new Error("Nanocodex root container is missing");
const directSurface = surfaceFromUrl(directUrl);
const directRepositorySurface = directSurface === "code" || directSurface === "commits"
  ? directSurface
  : undefined;
if (directRepositorySurface) {
  const commit = directUrl.searchParams.get("commit")?.toLowerCase();
  const requestedCommit = directRepositorySurface === "commits"
    && commit
    && /^[0-9a-f]{40}$/.test(commit)
    ? commit
    : undefined;
  void prepareRepositorySurface(directRepositorySurface, requestedCommit).catch(() => undefined);
}

createRoot(container).render(
  directPath === "/artifact-runtime"
    ? <ArtifactRuntime />
    : <BrowserApplication url={directUrl} />,
);

function BrowserApplication({ url }: { url: URL }) {
  const [preparedRoute, setPreparedRoute] = useState<PreparedDirectRoute | null>(
    directRepositorySurface || directSurface === "home" || directSurface === "agent" ? {} : null,
  );

  useEffect(() => {
    if (directRepositorySurface) return;
    let active = true;
    void preloadDirectSurface(url).then(
      (prepared) => {
        if (active) setPreparedRoute(prepared);
      },
      () => {
        if (active) setPreparedRoute({});
      },
    );
    return () => {
      active = false;
    };
  }, [url]);

  if (!preparedRoute) return null;
  return (
    <BrowserRouter useTransitions={false}>
      <Suspense fallback={null}>
        <AccountSessionProvider>
          <NanocodexApp preparedRoute={preparedRoute} />
        </AccountSessionProvider>
      </Suspense>
    </BrowserRouter>
  );
}
