import { Component, type ErrorInfo, type ReactNode } from "react";
import { t } from "../i18n";
import { showWindowWhenReady } from "../nativeFeel";

/**
 * Last line of defence for the UI: without it, one error while drawing any part of the window makes React
 * remove everything, and the user is left with an empty dark window. Instead, say what happened and offer
 * to redraw. Downloads are not affected: they run in the engine, not in this page.
 */
export class ErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("VeloxFetch UI error:", error, info.componentStack);
    showWindowWhenReady(); // the window may still be hidden if the very first drawing failed
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <div className="flex h-full items-center justify-center p-6" data-testid="ui-error">
        <div className="card max-w-lg p-6">
          <h1 className="text-lg font-semibold">{t("err.title")}</h1>
          <p className="mt-2 text-sm text-dim">{t("err.body")}</p>
          <pre className="selectable mt-3 max-h-40 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-line p-2 text-xs" data-testid="ui-error-text">
            {String(error.message || error).slice(0, 500)}
          </pre>
          <button className="btn btn-primary mt-4" data-testid="ui-error-retry" onClick={() => this.setState({ error: null })}>
            {t("err.redraw")}
          </button>
        </div>
      </div>
    );
  }
}
