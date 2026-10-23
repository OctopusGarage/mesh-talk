// @vitest-environment jsdom
import "@/lib/i18n";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { useAuth } from "@/store/auth";
import { ChannelHistorySettings } from "./ChannelHistorySettings";
const api = vi.hoisted(() => ({
  channelMembers: vi.fn(),
  setChannelHistoryPolicy: vi.fn(),
}));
vi.mock("@/lib/api", () => ({ chat: api, auth: {} }));
let host: HTMLDivElement;
let root: Root;
const info = (owner = "me", pending = false) => ({
  owner,
  history_policy: "last_ten_within_three_days",
  members: [{ user_id: "me", pending }],
});
beforeEach(() => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  useAuth.setState({
    user: { id: "owner", username: "me", display_name: "Me" },
    generation: 1,
  });
  api.channelMembers.mockReset().mockResolvedValue(info());
  api.setChannelHistoryPolicy.mockReset().mockResolvedValue(undefined);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});
async function render(
  channelId = "a",
  dissolved = false,
  channelOwner = "me",
  selfAccepted = true,
) {
  await act(async () =>
    root.render(
      createElement(ChannelHistorySettings, {
        channelId,
        myId: "me",
        dissolved,
        channelOwner,
        selfAccepted,
      }),
    ),
  );
}
async function change(value: string) {
  await act(async () => {
    const select = host.querySelector("select")!;
    select.value = value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}
it("loads the owner policy and saves the selected history window", async () => {
  await render();
  expect(host.querySelector("select")?.value).toBe(
    "last_ten_within_three_days",
  );
  await change("off");
  expect(api.setChannelHistoryPolicy).toHaveBeenCalledWith("a", "off");
  expect(host.querySelector("select")?.value).toBe("off");
});
it.each([
  ["other", false, false],
  ["me", true, false],
  ["me", false, true],
])(
  "shows a read-only policy for owner %s pending %s dissolved %s",
  async (owner, pending, dissolved) => {
    api.channelMembers.mockResolvedValue(info(owner, pending));
    await render("a", dissolved);
    expect(host.querySelector("select")).toBeNull();
    expect(host.textContent).toContain(
      "Latest 10 messages from the last 3 days",
    );
  },
);
it("keeps the last saved policy and reports a failed change", async () => {
  await render();
  api.setChannelHistoryPolicy.mockRejectedValueOnce(new Error("offline"));
  await change("off");
  expect(host.querySelector("select")?.value).toBe(
    "last_ten_within_three_days",
  );
  expect(host.querySelector('[role="alert"]')).not.toBeNull();
});
it("disables editing while loading and offers retry after a load failure", async () => {
  api.channelMembers.mockRejectedValueOnce(new Error("offline"));
  await render();
  expect(host.querySelector("select")).toBeNull();
  expect(host.querySelector('[role="alert"]')).not.toBeNull();
  await act(async () => host.querySelector("button")!.click());
  expect(host.querySelector("select")).not.toBeNull();
});
it("does not apply an old channel save to a replacement channel", async () => {
  await render();
  let resolve!: () => void;
  api.setChannelHistoryPolicy.mockImplementationOnce(
    () =>
      new Promise<void>((r) => {
        resolve = r;
      }),
  );
  await change("off");
  expect(host.querySelector("select")?.disabled).toBe(true);
  await render("b");
  await act(async () => resolve());
  expect(host.querySelector("select")?.value).toBe(
    "last_ten_within_three_days",
  );
});
it("does not publish a failed save after the login generation changes", async () => {
  await render();
  let reject!: (error: Error) => void;
  api.setChannelHistoryPolicy.mockImplementationOnce(
    () =>
      new Promise<void>((_, r) => {
        reject = r;
      }),
  );
  await change("off");
  await act(async () => useAuth.setState({ generation: 2 }));
  await act(async () => reject(new Error("retired")));
  expect(host.querySelector('[role="alert"]')).toBeNull();
  expect(host.querySelector("select")?.disabled).toBe(false);
});
it("ignores a previous channel's delayed metadata after navigation", async () => {
  let resolve!: (value: ReturnType<typeof info>) => void;
  api.channelMembers.mockImplementationOnce(
    () =>
      new Promise<ReturnType<typeof info>>((r) => {
        resolve = r;
      }),
  );
  await render("a");
  expect(host.querySelector("select")).toBeNull();
  expect(host.querySelector('[role="status"]')).not.toBeNull();
  await render("b");
  await act(async () => resolve({ ...info(), history_policy: "off" }));
  expect(host.querySelector("select")?.value).toBe(
    "last_ten_within_three_days",
  );
});

it("refreshes after live ownership transfer and ignores a retired owner's save", async () => {
  await render();
  let finish!: () => void;
  api.setChannelHistoryPolicy.mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
  );
  await change("off");
  api.channelMembers.mockResolvedValue(info("other"));
  await render("a", false, "other");
  expect(host.querySelector("select")).toBeNull();
  await act(async () => finish());
  expect(host.querySelector("select")).toBeNull();
  expect(host.textContent).toContain("Latest 10 messages from the last 3 days");
  api.channelMembers.mockResolvedValue(info());
  await render("a", false, "me");
  expect(host.querySelector("select")).not.toBeNull();
  expect(api.channelMembers).toHaveBeenCalledTimes(3);
});
it("blocks editing when live membership becomes pending even if fetched metadata lags", async () => {
  await render();
  await render("a", false, "me", false);
  expect(host.querySelector("select")).toBeNull();
  expect(api.channelMembers).toHaveBeenCalledTimes(2);
});
