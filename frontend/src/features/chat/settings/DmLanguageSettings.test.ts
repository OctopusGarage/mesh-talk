// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({
  get: vi.fn(),
  set: vi.fn(),
  auth: { user: { id: "alice" } as { id: string } | null, generation: 1 },
}));
vi.mock("@/store/auth", () => ({
  useAuth: Object.assign(
    (select: (state: typeof mocks.auth) => unknown) => select(mocks.auth),
    { getState: () => mocks.auth },
  ),
}));
vi.mock("@/lib/api/language", () => ({
  dmLanguage: { get: mocks.get, set: mocks.set },
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
import { DmLanguageSettings } from "./DmLanguageSettings";
const policy = (language: string | null) => ({
  language,
  min_target_percent: 80,
  supported_languages: [
    "en",
    "zh",
    "ja",
    "es",
    "fr",
    "de",
    "ko",
    "pt",
    "ru",
    "ar",
  ],
});
let root: Root;
let host: HTMLDivElement;
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => {
    resolve = yes;
  });
  return { promise, resolve };
}
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.auth = { user: { id: "alice" }, generation: 1 };
  mocks.get.mockReset().mockResolvedValue(policy("ja"));
  mocks.set.mockReset();
  host = document.createElement("div");
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
async function render() {
  await act(async () => root.render(createElement(DmLanguageSettings)));
}
function select() {
  return host.querySelector<HTMLSelectElement>("#setting-dm-language")!;
}
async function choose(language: string) {
  await act(async () => {
    select().value = language;
    select().dispatchEvent(new Event("change", { bubbles: true }));
  });
}
it("loads the signed preference and offers unrestricted plus ten languages", async () => {
  await render();
  expect(mocks.get).toHaveBeenCalledExactlyOnceWith("alice");
  expect(select().value).toBe("ja");
  expect(select().options.length).toBe(11);
  expect(select().getAttribute("aria-describedby")).toBe(
    "setting-dm-language-help",
  );
  expect(host.textContent).toContain("dmLanguage.help");
});
it("waits for canonical save and shows saved state", async () => {
  const job = deferred<ReturnType<typeof policy>>();
  mocks.set.mockReturnValue(job.promise);
  await render();
  await choose("en");
  expect(mocks.set).toHaveBeenCalledExactlyOnceWith("alice", "en");
  expect(select().disabled).toBe(true);
  expect(select().value).toBe("ja");
  await act(async () => job.resolve(policy("en")));
  expect(select().value).toBe("en");
  expect(host.querySelector('[role="status"]')?.textContent).toBe(
    "settings.saved",
  );
});
it("can turn off the restriction", async () => {
  mocks.set.mockResolvedValue(policy(null));
  await render();
  await choose("");
  expect(mocks.set).toHaveBeenCalledWith("alice", null);
  expect(select().value).toBe("");
});
it("keeps the saved preference when a write fails", async () => {
  mocks.set.mockRejectedValue(new Error("unavailable"));
  await render();
  await choose("en");
  expect(select().value).toBe("ja");
  expect(select().disabled).toBe(false);
  expect(host.querySelector('[role="alert"]')?.textContent).toContain(
    "dmLanguage.saveFailed",
  );
});
it("offers retry after a load failure", async () => {
  mocks.get.mockRejectedValueOnce(new Error("offline"));
  await render();
  expect(select().disabled).toBe(true);
  expect(host.querySelector('[role="alert"]')?.textContent).toContain(
    "dmLanguage.loadFailed",
  );
  await act(async () => host.querySelector("button")!.click());
  expect(select().value).toBe("ja");
  expect(mocks.get).toHaveBeenCalledTimes(2);
});
it.each(["load", "save"])(
  "isolates a pending %s when the owner changes",
  async (kind) => {
    const job = deferred<ReturnType<typeof policy>>();
    if (kind === "load") mocks.get.mockReturnValueOnce(job.promise);
    await render();
    if (kind === "save") {
      mocks.set.mockReturnValueOnce(job.promise);
      await choose("en");
    }
    mocks.auth = { user: { id: "bob" }, generation: 2 };
    mocks.get.mockResolvedValue(policy("fr"));
    await render();
    await act(async () => job.resolve(policy("en")));
    expect(select().value).toBe("fr");
    expect(mocks.get).toHaveBeenLastCalledWith("bob");
    expect(host.querySelector('[role="status"]')).toBeNull();
  },
);
