// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { setLanguage } from "@/lib/i18n";
import { chat, type DeviceSyncStatus } from "@/lib/api/chat";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { usePresence } from "@/store/presence";
import { PersonalHeader } from "./ConversationHeaders";

vi.mock("./LinkDeviceDialog", () => ({
  LinkDeviceDialog: () => createElement("button", null, "Link a device"),
}));
let host: HTMLDivElement;
let root: Root;
const status = (
  device_id: string,
  phase: DeviceSyncStatus["phase"] = "offline",
): DeviceSyncStatus => ({
  device_id,
  phase,
  pending_attachments: 0,
  error: null,
});
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  setLanguage("en");
  useAuth.setState({
    user: { id: "owner" } as NonNullable<
      ReturnType<typeof useAuth.getState>["user"]
    >,
    generation: 1,
  });
  useChat.setState({
    myId: "this-device",
    myAccountId: "our-account",
    peers: [],
    ready: true,
  });
  usePresence.setState({ map: {}, unavailable: false });
  vi.spyOn(chat, "getDeviceSyncStatus").mockResolvedValue([]);
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
async function render() {
  await act(async () => root.render(createElement(PersonalHeader)));
}
async function open() {
  const button = host.querySelector<HTMLButtonElement>(
    '[data-testid="personal-devices-trigger"]',
  );
  expect(button).not.toBeNull();
  await act(async () => button!.click());
  return document.querySelector<HTMLElement>(
    '[data-testid="personal-devices"]',
  )!;
}
it("keeps device details collapsed by default, with a useful single-device state", async () => {
  await render();
  expect(document.querySelector('[data-testid="personal-devices"]')).toBeNull();
  const panel = await open();
  expect(panel.textContent).toContain("This device");
  expect(panel.textContent).toContain("No other linked devices yet");
  expect(panel.textContent).toContain("Link a device");
});
it("shows offline certified devices and uses device presence independently of sync phase", async () => {
  vi.mocked(chat.getDeviceSyncStatus).mockResolvedValue([
    status("online-device", "disabled"),
    status("old-device", "up_to_date"),
  ]);
  useChat.setState({
    peers: [
      {
        user_id: "online-device",
        account_id: "our-account",
        name: "Laptop",
        addr: "",
        post_office: false,
      },
      {
        user_id: "unrelated-device",
        account_id: "other-account",
        name: "Other person",
        addr: "",
        post_office: false,
      },
    ],
  });
  usePresence.setState({
    map: {
      "our-account": {
        online: true,
        last_seen_secs: 0,
        devices: [
          {
            user_id: "online-device",
            evidence: [
              {
                source: "authenticated",
                last_seen_secs: 0,
                expires_in_secs: 20,
              },
            ],
          },
        ],
      },
    },
  });
  await render();
  const panel = await open();
  expect(panel.textContent).toContain("Laptop");
  expect(panel.textContent).toContain("old-device");
  expect(panel.textContent).not.toContain("Other person");
  expect(
    panel.querySelector('[data-device-id="online-device"]')?.textContent,
  ).toContain("Online");
  expect(
    panel.querySelector('[data-device-id="old-device"]')?.textContent,
  ).toContain("Offline");
});
it("reports roster errors without claiming there are no other linked devices", async () => {
  vi.mocked(chat.getDeviceSyncStatus).mockRejectedValue(
    new Error("unavailable"),
  );
  await render();
  const panel = await open();
  expect(panel.textContent).toContain("Could not load linked devices");
  expect(panel.textContent).not.toContain("No other linked devices yet");
});
it("retires a pending roster request when the account changes", async () => {
  let resolve!: (value: DeviceSyncStatus[]) => void;
  vi.mocked(chat.getDeviceSyncStatus).mockReturnValueOnce(
    new Promise((done) => {
      resolve = done;
    }),
  );
  await render();
  await act(async () =>
    useChat.setState({ myAccountId: "new-account", myId: "new-device" }),
  );
  await act(async () => resolve([status("previous-owner-device")]));
  const panel = await open();
  expect(panel.textContent).toContain("new-device");
  expect(panel.textContent).not.toContain("previous-owner-device");
});

it("removes account device details immediately on logout", async () => {
  vi.mocked(chat.getDeviceSyncStatus).mockResolvedValue([
    status("private-device"),
  ]);
  await render();
  await open();
  await act(async () => useAuth.setState({ user: null, generation: 2 }));
  expect(
    host.querySelector('[data-testid="personal-devices-trigger"]'),
  ).toBeNull();
  expect(document.querySelector('[data-testid="personal-devices"]')).toBeNull();
});
