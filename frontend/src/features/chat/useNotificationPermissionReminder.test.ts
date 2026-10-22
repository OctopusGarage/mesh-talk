// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { useChat } from "@/store/chat";
import { useSettings } from "@/store/settings";
import { useNotificationPermission } from "@/lib/notify";
import type { SendIntent, SendPayload } from "@/store/conversationModel";
import { useNotificationPermissionReminder } from "./useNotificationPermissionReminder";
import { NotificationPermissionNotice } from "./NotificationPermissionNotice";

const mocks = vi.hoisted(() => ({
  check: vi.fn(),
  request: vi.fn(),
  openSettings: vi.fn(),
}));
vi.mock("@/lib/api/notifications", () => ({ notifications: mocks }));
vi.mock("@/store/chat", async () => {
  const { create } = await import("zustand");
  return {
    useChat: create(() => ({ ready: true, intents: {}, myAccountId: "self" })),
  };
});
vi.mock("@/store/settings", async () => {
  const { create } = await import("zustand");
  return { useSettings: create(() => ({ notificationsEnabled: true })) };
});
const denied = { status: "denied", platform: "macos", can_open_settings: true };
let host: HTMLDivElement;
let root: Root;
let nextId: number;
function Probe({ identity }: { identity: string }) {
  const reminder = useNotificationPermissionReminder(identity);
  return reminder.visible
    ? createElement(NotificationPermissionNotice, {
        onDismiss: reminder.dismiss,
      })
    : null;
}
async function render(identity = "alice:1") {
  await act(async () => root.render(createElement(Probe, { identity })));
}
function intent(payload: SendPayload, account = "peer"): SendIntent {
  return {
    conversation: { kind: "account", id: account, name: account },
    payload,
    message: {
      clientId: `c${++nextId}`,
      id: null,
      fromMe: true,
      who: "Alice",
      text: "Submitted",
      wallClock: 1,
      replyTo: null,
    },
  };
}
async function submit(
  payload: SendPayload = { kind: "text", text: "Hello", replyTo: null },
  account = "peer",
) {
  const value = intent(payload, account);
  await act(async () =>
    useChat.setState((s) => ({
      intents: { ...s.intents, [value.message.clientId!]: value },
    })),
  );
  return value;
}
const notice = () =>
  host.querySelector('[data-testid="notification-permission-notice"]');
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.resetAllMocks();
  await i18n.changeLanguage("en");
  nextId = 0;
  mocks.check.mockResolvedValue(denied);
  useChat.setState({ ready: true, intents: {}, myAccountId: "self" });
  useSettings.setState({ notificationsEnabled: true });
  useNotificationPermission.setState({
    permission: null,
    checking: false,
    requesting: false,
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});
it.each<SendPayload>([
  { kind: "text", text: "Hello", replyTo: null },
  { kind: "sticker", stickerId: "wave", fallback: "Wave" },
  { kind: "file", path: "/tmp/photo.png", media: true },
])(
  "offers a reply-notification reminder for an admitted $kind intent",
  async (payload) => {
    await render();
    expect(notice()).toBeNull();
    await submit(payload);
    expect(notice()?.textContent).toContain("Messages still send");
    expect(mocks.request).not.toHaveBeenCalled();
  },
);
it("retains dismissal across conversation navigation but allows a new login generation to be reminded", async () => {
  await render();
  await submit();
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        'button[aria-label="Dismiss notification reminder"]',
      )!
      .click(),
  );
  await act(async () =>
    useChat.setState({
      active: { kind: "account", id: "another-peer", name: "Another peer" },
    }),
  );
  await render();
  await submit();
  expect(notice()).toBeNull();
  await render("alice:2");
  expect(notice()).toBeNull();
  await submit();
  expect(notice()).not.toBeNull();
});
it("does not treat Saved Messages or updates to existing intents as a new reply-notification opportunity", async () => {
  await render();
  const value = await submit(undefined, "self");
  await act(async () =>
    useChat.setState({
      intents: {
        [value.message.clientId!]: {
          ...value,
          message: { ...value.message, id: "accepted" },
        },
      },
    }),
  );
  expect(notice()).toBeNull();
  expect(mocks.check).toHaveBeenCalledOnce();
});
it("does not publish an old session's delayed reminder into its replacement", async () => {
  let resolve!: (value: unknown) => void;
  mocks.check.mockReturnValue(
    new Promise((done) => {
      resolve = done;
    }),
  );
  await render();
  await submit();
  await render("bob:2");
  await act(async () => {
    resolve(denied);
    await useNotificationPermission.getState().refresh();
  });
  expect(notice()).toBeNull();
});
it("does not inspect permission when the account has disabled notifications", async () => {
  useSettings.setState({ notificationsEnabled: false });
  await render();
  await submit();
  expect(notice()).toBeNull();
  expect(mocks.check).not.toHaveBeenCalled();
});
it("hides the reminder and removes observers when notifications are disabled", async () => {
  await render();
  await submit();
  expect(notice()).not.toBeNull();
  await act(async () => useSettings.setState({ notificationsEnabled: false }));
  const checks = mocks.check.mock.calls.length;
  await act(async () => {
    window.dispatchEvent(new Event("focus"));
    document.dispatchEvent(new Event("visibilitychange"));
  });
  expect(notice()).toBeNull();
  expect(mocks.check).toHaveBeenCalledTimes(checks);
});
it("rechecks on visible return and focus, then clears the reminder after a system grant", async () => {
  await render();
  await submit();
  const visibility = vi
    .spyOn(document, "visibilityState", "get")
    .mockReturnValue("hidden");
  const checks = mocks.check.mock.calls.length;
  await act(async () => document.dispatchEvent(new Event("visibilitychange")));
  expect(mocks.check).toHaveBeenCalledTimes(checks);
  visibility.mockReturnValue("visible");
  mocks.check.mockResolvedValue({ ...denied, status: "granted" });
  await act(async () => document.dispatchEvent(new Event("visibilitychange")));
  expect(notice()).toBeNull();
  mocks.check.mockResolvedValue(denied);
  await act(async () => window.dispatchEvent(new Event("focus")));
  expect(notice()).not.toBeNull();
});
it.each(["granted", "unknown"])(
  "does not present %s permission as blocked",
  async (status) => {
    mocks.check.mockResolvedValue({ ...denied, status });
    await render();
    await submit();
    expect(notice()).toBeNull();
  },
);
