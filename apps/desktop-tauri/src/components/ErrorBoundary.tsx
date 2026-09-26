import { Component, type ErrorInfo, type ReactNode } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { revealTrayPanelWindow } from "../lib/tauri";
import RevealAfterPaint from "./RevealAfterPaint";

interface ErrorBoundaryProps {
  children: ReactNode;
  /** Reload the whole page. Defaults to `window.location.reload()`; injectable because jsdom's `location.reload` is neither implemented nor spy-able. */
  reload?: () => void;
}

interface ErrorBoundaryState {
  hasError: boolean;
  /** Whatever was thrown — React does not guarantee an `Error` instance. */
  error: unknown;
}

/** Human-readable form of a thrown value: `TypeError: …` for errors, `String(x)` otherwise. */
export function describeThrown(error: unknown): string {
  try {
    if (error instanceof Error) {
      return error.message ? `${error.name}: ${error.message}` : error.name;
    }
    return String(error);
  } catch {
    // The fallback renders with no boundary above it, so even an object with
    // hostile conversion methods must not make this function throw.
    try {
      return Object.prototype.toString.call(error);
    } catch {
      return "Unknown error";
    }
  }
}

/**
 * Root error boundary for every Ceiling window.
 *
 * React 18 unmounts the whole tree on an uncaught render error, which in a
 * Tauri window means the WebView goes blank with no message and no way back
 * short of restarting the app. On a `transparent: true` window (flyout,
 * main, float bar) that blank is literally invisible — the user sees the
 * desktop through an empty frame (#410, finding B). The fallback therefore
 * paints an opaque panel, names the error, and offers a retry and a reload.
 *
 * It sits above `LocaleProvider` (which lives inside `App`), so the
 * fallback deliberately uses plain English rather than `useLocale()`:
 * locale loading is itself a render path that can throw.
 */
export default class ErrorBoundary extends Component<
  ErrorBoundaryProps,
  ErrorBoundaryState
> {
  state: ErrorBoundaryState = { hasError: false, error: undefined };

  static getDerivedStateFromError(error: unknown): ErrorBoundaryState {
    return { hasError: true, error };
  }

  componentDidCatch(error: unknown, info: ErrorInfo): void {
    // Visible in the WebView2 DevTools console; the Rust side has no view
    // into render errors otherwise.
    console.error("[ErrorBoundary] uncaught render error", error, info.componentStack);

  }

  private handleRetry = (): void => {
    this.setState({ hasError: false, error: undefined });
  };

  private handleReload = (): void => {
    (this.props.reload ?? (() => window.location.reload()))();
  };

  render(): ReactNode {
    if (!this.state.hasError) {
      return this.props.children;
    }

    const fallback = (
      <div className="error-boundary" role="alert">
        <section className="panel error">
          <h2>Something went wrong</h2>
          <p>
            This Ceiling window hit an error while drawing. Try again first;
            if it keeps happening, reload, or quit and reopen Ceiling from the
            tray menu.
          </p>
          <pre>{describeThrown(this.state.error)}</pre>
          <div className="error-boundary__actions">
            <button type="button" className="btn btn--ghost" onClick={this.handleRetry}>
              Try again
            </button>
            <button type="button" className="btn btn--ghost" onClick={this.handleReload}>
              Reload
            </button>
          </div>
        </section>
      </div>
    );
    // A first-render error replaces the layout that normally releases a
    // hidden window. Paint the opaque fallback before revealing it.
    let label: string | null = null;
    try {
      label = getCurrentWebviewWindow().label;
    } catch {
      // A browser test or non-Tauri render has no native window to reveal.
    }
    if (label === "flyout") {
      return <RevealAfterPaint reveal={revealTrayPanelWindow}>{fallback}</RevealAfterPaint>;
    }
    if (label === "main" || label === "settings") {
      return <RevealAfterPaint>{fallback}</RevealAfterPaint>;
    }
    return fallback;
  }
}
