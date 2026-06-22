import { Component, type ErrorInfo, type ReactNode } from "react";

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
          <h1 className="text-lg font-semibold">Something went wrong</h1>
          <pre className="max-h-60 max-w-xl overflow-auto rounded-md bg-muted p-3 text-left text-xs text-destructive">
            {this.state.error.message}
          </pre>
          <button
            type="button"
            className="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground"
            onClick={() => window.location.reload()}
          >
            Reload
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
