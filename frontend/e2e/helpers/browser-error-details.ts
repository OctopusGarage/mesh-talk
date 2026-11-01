import type { Page } from "@playwright/test";

export interface BrowserErrorValue {
  name?: string;
  message: string;
  stack?: string;
  cause?: BrowserErrorValue;
  truncated?: boolean;
}

export interface BrowserErrorSnapshot {
  errors: Array<{
    error: BrowserErrorValue;
    filename?: string;
    line?: number;
    column?: number;
    eventType?: "error" | "unhandledrejection";
  }>;
  truncated: boolean;
  readError?: string;
}

const snapshots = new WeakMap<Page, BrowserErrorSnapshot>();
const bindingName = "__meshPortableBrowserErrorDetails";
const limits = { events: 20, text: 16_384, causeDepth: 4 };

export async function installBrowserErrorDetails(page: Page) {
  if (snapshots.has(page)) return;
  const snapshot: BrowserErrorSnapshot = { errors: [], truncated: false };
  snapshots.set(page, snapshot);
  await page.exposeBinding(bindingName, (_source, message) => {
    if (message.kind === "flush") return;
    if (
      message.kind === "overflow" ||
      snapshot.errors.length >= limits.events
    ) {
      snapshot.truncated = true;
      return;
    }
    snapshot.errors.push(message.record);
  });
  // Serialize in the browser before Firefox/Playwright flattens Error.cause.
  // The page binding retains plain data across navigation, with a global cap.
  await page.addInitScript(
    ({ sinkName, bounds }) => {
      let events = 0;
      function text(value: unknown): string {
        try {
          return String(value).slice(0, bounds.text);
        } catch {
          return "[unreadable value]";
        }
      }
      function serialize(
        value: unknown,
        depth: number,
        seen: Set<object>,
      ): BrowserErrorValue {
        if (depth >= bounds.causeDepth)
          return { message: "[cause depth limit]", truncated: true };
        if (
          (typeof value !== "object" || value === null) &&
          typeof value !== "function"
        )
          return { message: text(value) };
        const object = value as Record<string, unknown>;
        if (seen.has(object))
          return { message: "[circular cause]", truncated: true };
        seen.add(object);
        function field(key: string): unknown {
          try {
            return object[key];
          } catch {
            return `[unreadable ${key}]`;
          }
        }
        const message = field("message");
        const name = field("name");
        const stack = field("stack");
        const cause = field("cause");
        return {
          message: message === undefined ? text(value) : text(message),
          ...(name === undefined ? {} : { name: text(name) }),
          ...(stack === undefined ? {} : { stack: text(stack) }),
          ...(cause === undefined
            ? {}
            : { cause: serialize(cause, depth + 1, seen) }),
        };
      }
      function send(message: unknown) {
        try {
          const sink = Reflect.get(window, sinkName) as (
            value: unknown,
          ) => Promise<void>;
          // A disposed protocol channel must not create a second browser error.
          void sink(message).catch(() => undefined);
        } catch {
          // readBrowserErrorDetails records an unavailable channel separately.
        }
      }
      function capture(value: unknown, context: Record<string, unknown>) {
        if (events++ >= bounds.events) {
          if (events === bounds.events + 1) send({ kind: "overflow" });
          return;
        }
        try {
          send({
            kind: "error",
            record: { error: serialize(value, 0, new Set()), ...context },
          });
        } catch {
          send({
            kind: "error",
            record: {
              error: { message: "[unreadable browser error]" },
              ...context,
            },
          });
        }
      }
      window.addEventListener("error", (event) => {
        capture(event.error ?? event.message, {
          eventType: "error",
          filename: text(event.filename),
          line: event.lineno,
          column: event.colno,
        });
      });
      window.addEventListener("unhandledrejection", (event) => {
        capture(event.reason, { eventType: "unhandledrejection" });
      });
    },
    { sinkName: bindingName, bounds: limits },
  );
}

export async function readBrowserErrorDetails(
  page: Page,
): Promise<BrowserErrorSnapshot> {
  const snapshot = snapshots.get(page) ?? { errors: [], truncated: false };
  try {
    // A same-channel marker drains earlier synchronous binding callbacks without
    // an arbitrary sleep. Keep already delivered data even after page disposal.
    await page.evaluate(async (sinkName) => {
      const sink = Reflect.get(window, sinkName) as (
        value: unknown,
      ) => Promise<void>;
      await sink({ kind: "flush" });
    }, bindingName);
  } catch (error) {
    snapshot.readError = String(error).slice(0, limits.text);
  }
  return snapshot;
}
