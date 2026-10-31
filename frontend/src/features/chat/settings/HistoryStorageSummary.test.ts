// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { beforeEach, afterEach, it, expect, vi } from "vitest";
import i18n from "@/lib/i18n";
import { HistoryStorageSummary } from "./HistoryStorageSummary";
const mocks = vi.hoisted(() => ({ usage: vi.fn() }));
vi.mock("@/lib/api", () => ({ settings: { historyUsage: mocks.usage } }));
let root: Root;
let host: HTMLDivElement;
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.resetAllMocks();
  await i18n.changeLanguage("en");
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});
const render = async (identity: string | null = "alice:1") => {
  await act(async () =>
    root.render(createElement(HistoryStorageSummary, { identity })),
  );
};
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}
it("shows storage and a quiet review reminder without changing retention", async () => {
  mocks.usage.mockResolvedValue({ bytes: 2 * 1024 ** 3, complete: true });
  await render();
  expect(host.textContent).toContain("2 GiB");
  expect(host.textContent).toContain("Review your history retention");
  expect(mocks.usage).toHaveBeenCalledOnce();
  await render();
  expect(mocks.usage).toHaveBeenCalledOnce();
});
it("discards a delayed response after the settings owner changes", async () => {
  const old = deferred<{ bytes: number; complete: boolean }>();
  mocks.usage
    .mockReturnValueOnce(old.promise)
    .mockResolvedValueOnce({ bytes: 0, complete: true });
  await render();
  await render("bob:2");
  await act(async () => old.resolve({ bytes: 2 * 1024 ** 3, complete: true }));
  expect(host.textContent).toContain("0 B");
  expect(host.textContent).not.toContain("2 GiB");
});
it("labels partial metadata observations as a lower bound", async () => {
  mocks.usage.mockResolvedValue({ bytes: 1024, complete: false });
  await render();
  expect(host.textContent).toContain("At least 1 KiB");
});
it("retries errors and makes no request for a signed-out view", async () => {
  await render(null);
  expect(mocks.usage).not.toHaveBeenCalled();
  mocks.usage
    .mockRejectedValueOnce(new Error("disk unavailable"))
    .mockResolvedValueOnce({ bytes: 0, complete: true });
  await render();
  expect(host.textContent).toContain("Storage usage is unavailable");
  await act(async () =>
    host.querySelector<HTMLButtonElement>("button")!.click(),
  );
  expect(host.textContent).toContain("0 B");
});
