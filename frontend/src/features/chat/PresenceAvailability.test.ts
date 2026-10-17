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
  expect(html).toContain("已有在线信息将正常过期");
  expect(html).not.toContain("网络已断开");
  usePresence.setState({ unavailable: false });
  expect(renderToStaticMarkup(createElement(PresenceAvailability))).toBe("");
  setLanguage("en");
});
