import { Component, type ErrorInfo, type ReactNode } from "react";
import type { Surface } from "./navigation";

type RouteErrorBoundaryProps = {
  children: ReactNode;
  failure?: Error;
  surface: Surface;
};

type RouteErrorBoundaryState = {
  error: Error | null;
};

function surfaceLabel(surface: Surface): string {
  if (surface === "code") return "Source";
  return `${surface[0].toUpperCase()}${surface.slice(1)}`;
}

export class RouteErrorBoundary extends Component<
  RouteErrorBoundaryProps,
  RouteErrorBoundaryState
> {
  state: RouteErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): RouteErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("Nanocodex route failed", error, info.componentStack);
  }

  render() {
    const error = this.props.failure ?? this.state.error;
    if (!error) return this.props.children;
    const label = surfaceLabel(this.props.surface);

    return (
      <section className="requests-empty page-grid" role="alert">
        <p className="eyebrow">Nanocodex · {label}</p>
        <h1>{label} unavailable.</h1>
        <p>This route could not finish loading. Check your connection, then try again.</p>
        <button
          className="button button--medium"
          type="button"
          onClick={() => window.location.reload()}
        >
          Retry route
        </button>
      </section>
    );
  }
}
