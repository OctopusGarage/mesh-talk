import {
  test,
  expect,
  requireNoUncaughtBrowserErrors,
} from "./helpers/portable-evidence";
import {
  installBrowserErrorDetails,
  readBrowserErrorDetails,
} from "./helpers/browser-error-details";
import { enterChat } from "./helpers/session";

// Full Chromium acceptance extracts evidence from successful scenarios too.
test.use({ trace: "on" });

test("browser evidence retains nested causes and preserves the strict error gate", async ({
  page,
  browser,
}, testInfo) => {
  await enterChat(page);
  const probe = await browser.newPage();
  try {
    await installBrowserErrorDetails(probe);
    const protocolErrors: string[] = [];
    probe.on("pageerror", (error) => protocolErrors.push(error.message));
    const uncaught = probe.waitForEvent("pageerror");
    await probe.goto(
      `data:text/html,${encodeURIComponent('<script>throw new Error("outer recovery marker", { cause: new TypeError("original render failure") });</script>')}`,
    );
    const error = await uncaught;
    expect(() => requireNoUncaughtBrowserErrors([error.message])).toThrow(
      "uncaught React/browser errors",
    );
    const first = await readBrowserErrorDetails(probe);
    expect(first.errors[0]?.error.message).toBe("outer recovery marker");
    expect(first.errors[0]?.error.cause?.message).toBe(
      "original render failure",
    );
    expect(first.errors[0]?.error.cause?.stack).toContain("TypeError");

    // The Node-side evidence survives navigation; only bounded plain data crosses
    // the protocol, before Firefox can discard the original Error.cause.
    await probe.goto("data:text/html,<body>next document</body>");
    const rejection = probe.waitForEvent("pageerror");
    await probe.evaluate(() => {
      void Promise.reject(
        Object.defineProperty(new Error("outer promise rejection"), "cause", {
          value: new Error("original promise cause"),
        }),
      );
    });
    const rejected = await rejection;
    expect(() => requireNoUncaughtBrowserErrors([rejected.message])).toThrow(
      "uncaught React/browser errors",
    );
    const promised = await readBrowserErrorDetails(probe);
    expect(promised.errors[1]?.eventType).toBe("unhandledrejection");
    expect(promised.errors[1]?.error.cause?.message).toBe(
      "original promise cause",
    );
    await probe.evaluate(() => {
      const cycle = new Error("circular cause");
      Object.defineProperty(cycle, "cause", { value: cycle });
      window.dispatchEvent(new ErrorEvent("error", { error: cycle }));
      let deep = new Error("deep origin");
      for (let index = 0; index < 10; index++)
        deep = Object.defineProperty(new Error(`layer-${index}`), "cause", {
          value: deep,
        });
      window.dispatchEvent(new ErrorEvent("error", { error: deep }));
      window.dispatchEvent(
        new ErrorEvent("error", { error: new Error("x".repeat(30_000)) }),
      );
      const unreadable = Object.defineProperty({}, "message", {
        get() {
          throw new Error("message getter must not escape diagnostics");
        },
      });
      window.dispatchEvent(new ErrorEvent("error", { error: unreadable }));
      for (let index = 0; index < 30; index++)
        window.dispatchEvent(
          new ErrorEvent("error", { error: new Error(`overflow-${index}`) }),
        );
    });
    const bounded = await readBrowserErrorDetails(probe);
    expect(bounded.errors).toHaveLength(20);
    expect(bounded.truncated).toBe(true);
    expect(bounded.errors[0].error.cause?.message).toBe(
      "original render failure",
    );
    expect(
      bounded.errors.find((record) => record.error.message === "circular cause")
        ?.error.cause?.truncated,
    ).toBe(true);
    let cause = bounded.errors.find(
      (record) => record.error.message === "layer-9",
    )!.error;
    for (let depth = 0; cause.cause; depth++) {
      expect(depth).toBeLessThan(5);
      cause = cause.cause;
    }
    expect(cause.truncated).toBe(true);
    expect(
      bounded.errors.find((record) => record.error.message.startsWith("xxx"))
        ?.error.message.length,
    ).toBeLessThanOrEqual(16_384);
    expect(
      bounded.errors.some((record) =>
        record.error.message.includes("unreadable"),
      ),
    ).toBe(true);
    expect(protocolErrors).toHaveLength(2);
    await probe.close();
    const disposed = await readBrowserErrorDetails(probe);
    expect(disposed.errors).toHaveLength(20);
    expect(disposed.readError).toBeTruthy();
    await testInfo.attach("browser-error-control", {
      body: Buffer.from(
        JSON.stringify({ strictGateRejected: true, captured: disposed }),
      ),
      contentType: "application/json",
    });
  } finally {
    if (!probe.isClosed()) await probe.close();
  }
});
