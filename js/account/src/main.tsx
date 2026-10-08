import { QueryClientProvider } from "@tanstack/react-query";
import { appQueryClient } from "./queryClient";
import { lazy, Suspense, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router";
import { AccountSessionProvider } from "./AccountSession";
import { NanocodexApp } from "./NanocodexApp";
import type { PreparedDirectRoute } from "./routeLoaders";
import { surfaceFromUrl } from "./navigation";

const ArtifactRuntime = lazy(() => import("./artifactRuntime").then((module) => ({ default: module.ArtifactRuntime })));
const PermissionRequestPage = lazy(() => import("./PermissionRequestPage").then(module => ({ default: module.PermissionRequestPage })));
const BrowserLoginPage = lazy(() => import("./BrowserLoginPage").then(module => ({default:module.BrowserLoginPage})));
const SharedThreadView = lazy(() => import("./SharedThreadView").then((module) => ({ default: module.SharedThreadView })));
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
const needsDirectData = directSurface === "changelog" || directSurface === "docs"
  || (directSurface === "evals" && directPath === "/evals");
if (directRepositorySurface) {
  const commit = directUrl.searchParams.get("commit")?.toLowerCase();
  const requestedCommit = directRepositorySurface === "commits"
    && commit
    && /^[0-9a-f]{40}$/.test(commit)
    ? commit
    : undefined;
  void import("./routeLoaders")
    .then(({ prepareRepositorySurface }) => prepareRepositorySurface(directRepositorySurface, requestedCommit))
    .catch(() => undefined);
}

createRoot(container).render(
  directPath === "/artifact-runtime"
    ? <Suspense fallback={null}><ArtifactRuntime /></Suspense>
    : /^\/share\/[^/]+$/.test(directPath)
      ? <Suspense fallback={<p role="status">Opening shared thread…</p>}><SharedThreadView key={directPath} agentId={decodeURIComponent(directPath.slice(7))} /></Suspense>
      : <BrowserApplication url={directUrl} />,
);

function BrowserApplication({ url }: { url: URL }) {
  const [preparedRoute, setPreparedRoute] = useState<PreparedDirectRoute | null>(
    needsDirectData ? null : {},
  );

  useEffect(() => {
    if (!needsDirectData) return;
    let active = true;
    void import("./routeLoaders").then(({ preloadDirectSurface }) => preloadDirectSurface(url)).then(
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
    <QueryClientProvider client={appQueryClient}>
      <BrowserRouter useTransitions={false}>
        <Suspense fallback={null}>
          <AccountSessionProvider>
            {url.pathname === "/" && url.searchParams.has("permission_request") ? <PermissionRequestPage url={url} /> : url.pathname === "/browser-login" ? <BrowserLoginPage url={url} /> : <NanocodexApp preparedRoute={preparedRoute} />}
          </AccountSessionProvider>
        </Suspense>
      </BrowserRouter>
    </QueryClientProvider>
  );
}
