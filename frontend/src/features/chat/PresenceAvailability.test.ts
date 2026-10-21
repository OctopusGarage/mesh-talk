import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";
import { setLanguage } from "@/lib/i18n";
import { usePresence } from "@/store/presence";
import { PresenceAvailability } from "./PresenceAvailability";

vi.mock("@/store/presence", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/store/presence")>();
  return {
    ...actual,
    usePresence: Object.assign(
      (
        select: (s: ReturnType<typeof actual.usePresence.getState>) => unknown,
      ) => select(actual.usePresence.getState()),
      actual.usePresence,
    ),
  };
});

it("announces unavailable presence without claiming Internet or peer sign-out", () => {
  setLanguage("zh-Hans");
  usePresence.setState({ unavailable: true });
  const html = renderToStaticMarkup(createElement(PresenceAvailability));
  expect(html).toContain('role="status"');
  expect(html).toContain("在线状态暂无法更新");
  expect(html).toContain("正在自动重试");
  expect(html).toContain("在线标记可能已过时");
  expect(html).toContain("<summary");
  expect(html).not.toContain("Retained observations");
  expect(html).not.toContain("网络已断开");
  usePresence.setState({ unavailable: false });
  expect(renderToStaticMarkup(createElement(PresenceAvailability))).toBe("");
  setLanguage("en");
});

it.each([
  [
    { kind: "request", message: "discovery unavailable <script>" } as const,
    "discovery unavailable &lt;script&gt;",
  ],
  [
    { kind: "timeout" } as const,
    "The online status request timed out after 10 seconds.",
  ],
  [
    { kind: "invalid" } as const,
    "The online status response could not be read.",
  ],
])(
  "keeps diagnostic failure details inside the disclosure: %s",
  (failure, message) => {
    setLanguage("en");
    usePresence.setState({ unavailable: true, failure });
    const html = renderToStaticMarkup(createElement(PresenceAvailability));
    expect(html).toContain("Latest error");
    expect(html).toContain(message);
    expect(html.indexOf(message)).toBeGreaterThan(html.indexOf("</summary>"));
    usePresence.setState({ unavailable: false, failure: null });
  },
);
