import { Component, type ErrorInfo, type ReactNode } from "react";
import i18n from "@/lib/i18n";

/**
 * Last-resort boundary so a render-time error surfaces as a readable message + a reload
 * button instead of an unrecoverable blank/black screen (the app forces dark mode, so an
 * unmounted tree just shows the near-black background).
 */
export class ErrorBoundary extends Component<
  { children: ReactNode },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // Keep a trace in the webview console / logs for diagnosis.
    console.error("Unhandled UI error:", error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      return (
        <div className="flex h-full flex-col items-center justify-center gap-4 p-8 text-center">
          <h1 className="font-display text-lg font-semibold">
            {i18n.t("errorBoundary.title")}
          </h1>
          <p className="max-w-md text-sm text-muted-foreground">
            {i18n.t("errorBoundary.description")}
          </p>
          <button
            type="button"
            className="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            onClick={() => window.location.reload()}
          >
            {i18n.t("errorBoundary.reload")}
          </button>
          <details className="max-w-md text-left text-xs text-muted-foreground">
            <summary className="cursor-pointer">
              {i18n.t("errorBoundary.details")}
            </summary>
            <pre className="mt-2 max-h-40 overflow-auto rounded-md bg-muted p-3 text-destructive">
              {this.state.error.message}
            </pre>
          </details>
        </div>
      );
    }
    return this.props.children;
  }
}
