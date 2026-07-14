import { Component, type ReactNode } from 'react';

interface RouteErrorBoundaryProps {
  /** Reset key — remount per surface so one bad view can't blank the app. */
  view: string;
  children: ReactNode;
}

interface RouteErrorBoundaryState {
  error: Error | null;
}

/** Route-level error boundary (handoff §Interactions #2): a crash inside one
 *  surface renders an inline failure panel; the rail, header, and every other
 *  surface stay usable. */
export class RouteErrorBoundary extends Component<
  RouteErrorBoundaryProps,
  RouteErrorBoundaryState
> {
  state: RouteErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): RouteErrorBoundaryState {
    return { error };
  }

  componentDidUpdate(prev: RouteErrorBoundaryProps) {
    if (prev.view !== this.props.view && this.state.error) {
      this.setState({ error: null });
    }
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <section className="route-error" role="alert">
        <span className="material-symbols-outlined" aria-hidden="true">
          error
        </span>
        <h2>This surface failed to render</h2>
        <p className="route-error-detail">{error.message}</p>
        <p className="muted">
          The rest of the app is unaffected — switch surfaces or retry.
        </p>
        <button type="button" onClick={() => this.setState({ error: null })}>
          Retry
        </button>
      </section>
    );
  }
}
