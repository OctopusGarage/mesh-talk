// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({
  get: vi.fn(),
  validate: vi.fn(),
  result: vi.fn(),
  auth: { user: { id: "alice" }, generation: 1 },
  chat: { active: { kind: "account", id: "peer" } },
}));
vi.mock("@/lib/api/language", () => ({
  dmLanguage: { get: mocks.get, validate: mocks.validate },
}));
vi.mock("@/store/auth", () => ({ useAuth: { getState: () => mocks.auth } }));
vi.mock("@/store/chat", () => ({ useChat: { getState: () => mocks.chat } }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, vars?: { language?: string }) =>
      vars?.language ? `${key}:${vars.language}` : key,
  }),
}));
import { useDmLanguagePolicy } from "./useDmLanguagePolicy";
function Probe({ account }: { account: string | null }) {
  const { language, validateText } = useDmLanguagePolicy(
    mocks.auth.user.id,
    mocks.auth.generation,
    account,
  );
  return createElement(
    "div",
    null,
    createElement("span", null, language),
    createElement(
      "button",
      {
        onClick: () => void validateText?.("hello").then(mocks.result),
        disabled: !validateText,
      },
      "send",
    ),
  );
}
let host: HTMLDivElement;
let root: Root;
function policy(language: string | null) {
  return {
    language,
    min_target_percent: 80,
    supported_languages: ["en", "ja"],
  };
}
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
  mocks.chat.active = { kind: "account", id: "peer" };
  mocks.get.mockReset().mockResolvedValue(policy("ja"));
  mocks.validate.mockReset();
  mocks.result.mockReset();
  host = document.createElement("div");
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
async function render(account: string | null = "peer") {
  await act(async () => root.render(createElement(Probe, { account })));
}
async function click() {
  await act(async () => host.querySelector("button")!.click());
}
it("reads the account policy once and exposes the language", async () => {
  await render();
  await render();
  expect(mocks.get).toHaveBeenCalledExactlyOnceWith("alice", "peer");
  expect(host.querySelector("span")!.textContent).toBe("ja");
});
it("does not read or validate group messages", async () => {
  await render(null);
  expect(mocks.get).not.toHaveBeenCalled();
  expect(host.querySelector("button")!.disabled).toBe(true);
});
it("returns the translated target requirement on rejection", async () => {
  mocks.validate.mockResolvedValue({
    allowed: false,
    language: "ja",
    target_percent: 40,
  });
  await render();
  await click();
  expect(mocks.validate).toHaveBeenCalledExactlyOnceWith(
    "alice",
    "peer",
    "hello",
  );
  expect(mocks.result).toHaveBeenCalledWith(
    "dmLanguage.rejected:dmLanguage.languages.ja",
  );
});
it("allows unrestricted messages without language warnings", async () => {
  mocks.validate.mockResolvedValue({
    allowed: true,
    language: "",
    target_percent: 100,
  });
  await render();
  await click();
  expect(mocks.result).toHaveBeenCalledWith(null);
});
it("fails closed on validation errors", async () => {
  mocks.validate.mockRejectedValue(new Error("offline"));
  await render();
  await click();
  expect(mocks.result).toHaveBeenCalledWith("dmLanguage.validationFailed");
});
it("ignores a stale policy read after changing accounts", async () => {
  const job = deferred<ReturnType<typeof policy>>();
  mocks.get.mockReturnValueOnce(job.promise);
  await render();
  mocks.chat.active.id = "other";
  mocks.get.mockResolvedValue(policy("en"));
  await render("other");
  await act(async () => job.resolve(policy("ja")));
  expect(host.querySelector("span")!.textContent).toBe("en");
});
it.each(["owner", "conversation"])(
  "rejects results when the %s changes during validation",
  async (kind) => {
    const job = deferred<{
      allowed: boolean;
      language: string;
      target_percent: number;
    }>();
    mocks.validate.mockReturnValue(job.promise);
    await render();
    await click();
    if (kind === "owner") mocks.auth.generation += 1;
    else mocks.chat.active.id = "other";
    await act(async () =>
      job.resolve({ allowed: true, language: "en", target_percent: 100 }),
    );
    expect(mocks.result).toHaveBeenCalledWith("dmLanguage.validationFailed");
  },
);

it("explains resource limits without reporting a language ratio failure", async () => {
  mocks.validate.mockResolvedValue({
    allowed: false,
    language: "en",
    target_percent: 0,
    reason: "resource-limit",
  });
  await render();
  await click();
  expect(mocks.result).toHaveBeenCalledWith("dmLanguage.resourceLimit");
});
it("refreshes the visible target when validation observes a changed policy", async () => {
  mocks.validate.mockResolvedValue({
    allowed: true,
    language: "en",
    target_percent: 100,
  });
  await render();
  expect(host.querySelector("span")!.textContent).toBe("ja");
  await click();
  expect(host.querySelector("span")!.textContent).toBe("en");
});

it("does not let an older policy read overwrite the validated target", async () => {
  const job = deferred<ReturnType<typeof policy>>();
  mocks.get.mockReturnValueOnce(job.promise);
  mocks.validate.mockResolvedValue({
    allowed: true,
    language: "en",
    target_percent: 100,
  });
  await render();
  await click();
  await act(async () => job.resolve(policy("ja")));
  expect(host.querySelector("span")!.textContent).toBe("en");
});
