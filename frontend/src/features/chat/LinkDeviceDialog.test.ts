// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  chat,
  type DeviceSyncPolicy,
  type DeviceSyncStatus,
} from "@/lib/api/chat";
import { auth } from "@/lib/api/auth";
import { setLanguage } from "@/lib/i18n";
import { publishAppearanceBeforeLink } from "@/lib/linkedAppearance";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { LinkDeviceDialog } from "./LinkDeviceDialog";

vi.mock("@/lib/linkedAppearance", () => ({
  publishAppearanceBeforeLink: vi.fn().mockResolvedValue(undefined),
}));

const policy = (history = false): DeviceSyncPolicy => ({
  history,
  attachments: false,
  profile: false,
  appearance: false,
  contacts: false,
});
const status = (device_id: string): DeviceSyncStatus => ({
  device_id,
  phase: "up_to_date",
  pending_attachments: 0,
  error: null,
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  setLanguage("en");
  useAuth.setState({
    user: { id: "owner-a" } as NonNullable<
      ReturnType<typeof useAuth.getState>["user"]
    >,
    generation: 1,
  });
  useChat.setState({
    myId: "this-device",
    myAccountId: "our-account",
    peers: [],
    ready: true,
    error: null,
  });
  vi.mocked(publishAppearanceBeforeLink)
    .mockReset()
    .mockResolvedValue(undefined);
  vi.spyOn(chat, "getDeviceSyncPolicy").mockResolvedValue(policy());
  vi.spyOn(chat, "getDeviceSyncStatus").mockResolvedValue([
    status("new-device"),
  ]);
  vi.spyOn(chat, "setDeviceSyncCategory").mockResolvedValue(policy(true));
  vi.spyOn(chat, "startLinking").mockResolvedValue("new-code");
  vi.spyOn(chat, "stopLinking").mockResolvedValue(undefined);
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
const dialog = () =>
  document.querySelector<HTMLElement>('[data-testid="link-device-dialog"]');
async function render() {
  await act(async () => root.render(createElement(LinkDeviceDialog)));
}
async function open() {
  const trigger = host.querySelector<HTMLButtonElement>(
    '[data-testid="sidebar-action-link"]',
  );
  expect(trigger).not.toBeNull();
  await act(async () => trigger!.click());
  expect(dialog()).not.toBeNull();
}
function historySwitch() {
  const button = dialog()?.querySelector<HTMLButtonElement>(
    '[role="switch"][aria-label="Message history"]',
  );
  expect(button).not.toBeNull();
  return button!;
}
async function replaceSession(owner: string, generation: number) {
  await act(async () =>
    useAuth.setState({
      user: { id: owner } as NonNullable<
        ReturnType<typeof useAuth.getState>["user"]
      >,
      generation,
    }),
  );
  expect(dialog()).toBeNull();
  await open();
}
async function showCode() {
  const button = dialog()?.querySelector<HTMLButtonElement>(
    '[data-testid="link-show-code"]',
  );
  expect(button).not.toBeNull();
  await act(async () => button!.click());
}

it.each([
  { name: "owner changes", owner: "owner-b", generation: 1 },
  {
    name: "the same owner starts a new generation",
    owner: "owner-a",
    generation: 2,
  },
])(
  "retires pending policy and status reads when $name",
  async ({ owner, generation }) => {
    const oldPolicy = deferred<DeviceSyncPolicy>();
    const oldStatus = deferred<DeviceSyncStatus[]>();
    vi.mocked(chat.getDeviceSyncPolicy).mockReturnValueOnce(oldPolicy.promise);
    vi.mocked(chat.getDeviceSyncStatus).mockReturnValueOnce(oldStatus.promise);
    await render();
    await open();
    expect(historySwitch().disabled).toBe(true);
    expect(chat.getDeviceSyncPolicy).toHaveBeenLastCalledWith("owner-a");
    expect(chat.getDeviceSyncStatus).toHaveBeenLastCalledWith("owner-a");

    await replaceSession(owner, generation);
    expect(historySwitch().getAttribute("aria-checked")).toBe("false");
    expect(chat.getDeviceSyncPolicy).toHaveBeenLastCalledWith(owner);
    expect(chat.getDeviceSyncStatus).toHaveBeenLastCalledWith(owner);
    await act(async () => {
      oldPolicy.resolve(policy(true));
      oldStatus.resolve([status("old-device")]);
    });
    expect(historySwitch().getAttribute("aria-checked")).toBe("false");
    expect(dialog()?.textContent).toContain("new-devi");
    expect(dialog()?.textContent).not.toContain("old-devi");
  },
);

it.each(["resolve", "reject"] as const)(
  "retires a pending category save after remount when it later %ss",
  async (outcome) => {
    const oldSave = deferred<DeviceSyncPolicy>();
    vi.mocked(chat.setDeviceSyncCategory).mockReturnValueOnce(oldSave.promise);
    await render();
    await open();
    await act(async () => historySwitch().click());
    expect(chat.setDeviceSyncCategory).toHaveBeenCalledWith(
      "history",
      true,
      "owner-a",
    );
    expect(historySwitch().disabled).toBe(true);

    await replaceSession("owner-b", 2);
    expect(historySwitch().disabled).toBe(false);
    await act(async () => {
      if (outcome === "resolve") oldSave.resolve(policy(true));
      else oldSave.reject(new Error("old owner save failed"));
    });
    expect(historySwitch().getAttribute("aria-checked")).toBe("false");
    expect(dialog()?.textContent).not.toContain("old owner save failed");
    await act(async () => historySwitch().click());
    expect(chat.setDeviceSyncCategory).toHaveBeenLastCalledWith(
      "history",
      true,
      "owner-b",
    );
    expect(historySwitch().getAttribute("aria-checked")).toBe("true");
  },
);

it("ignores an old pairing-code failure after a new owner displays its own code", async () => {
  const oldCode = deferred<string>();
  vi.mocked(chat.startLinking).mockReturnValueOnce(oldCode.promise);
  await render();
  await open();
  await showCode();
  expect(publishAppearanceBeforeLink).toHaveBeenCalledWith("owner-a");
  await replaceSession("owner-b", 2);
  await showCode();
  expect(publishAppearanceBeforeLink).toHaveBeenLastCalledWith("owner-b");
  const newCode = dialog()?.querySelector(
    '[data-testid="pairing-code"]',
  )?.textContent;
  expect(newCode?.replace(/ /g, "")).toBe("new-code");
  await act(async () => oldCode.reject(new Error("old pairing failed")));
  expect(
    dialog()?.querySelector('[data-testid="pairing-code"]')?.textContent,
  ).toBe(newCode);
  expect(dialog()?.textContent).not.toContain("old pairing failed");
});

it("does not start pairing after the appearance preparation belongs to a retired owner", async () => {
  const oldAppearance = deferred<void>();
  vi.mocked(publishAppearanceBeforeLink).mockReturnValueOnce(
    oldAppearance.promise,
  );
  await render();
  await open();
  await showCode();
  expect(chat.startLinking).not.toHaveBeenCalled();
  await replaceSession("owner-b", 2);
  await act(async () => oldAppearance.resolve());
  expect(chat.startLinking).not.toHaveBeenCalled();
  expect(dialog()?.querySelector('[data-testid="pairing-code"]')).toBeNull();
});

it.each([
  { name: "another owner", owner: "owner-b", generation: 1 },
  { name: "a newer generation", owner: "owner-a", generation: 2 },
])(
  "does not stop replacement pairing when an old code arrives for $name",
  async ({ owner, generation }) => {
    const oldCode = deferred<string>();
    vi.mocked(chat.startLinking).mockReturnValueOnce(oldCode.promise);
    await render();
    await open();
    await showCode();
    await replaceSession(owner, generation);
    await showCode();
    const currentCode = dialog()?.querySelector(
      '[data-testid="pairing-code"]',
    )?.textContent;
    expect(currentCode?.replace(/ /g, "")).toBe("new-code");
    await act(async () => oldCode.resolve("old-code"));
    expect(chat.stopLinking).not.toHaveBeenCalled();
    expect(
      dialog()?.querySelector('[data-testid="pairing-code"]')?.textContent,
    ).toBe(currentCode);
  },
);

it("does not adopt a linked account after its originating desktop session retires", async () => {
  const linking = deferred<string>();
  vi.spyOn(chat, "linkDevice").mockReturnValue(linking.promise);
  vi.spyOn(auth, "adoptLinkedAccount").mockResolvedValue(undefined);
  useChat.setState({
    peers: [
      {
        user_id: "pairing-device",
        account_id: "another-account",
        name: "Other",
        addr: "127.0.0.1:9",
        post_office: false,
      },
    ],
  });
  await render();
  await open();
  await act(async () => {
    const select =
      dialog()!.querySelector<HTMLSelectElement>("#link-device-peer")!;
    select.value = "pairing-device";
    select.dispatchEvent(new Event("change", { bubbles: true }));
    const input =
      dialog()!.querySelector<HTMLInputElement>("#link-device-code")!;
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, "pairing-code");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  const link = Array.from(
    dialog()!.querySelectorAll<HTMLButtonElement>("button"),
  ).find((button) => button.textContent === "Link");
  expect(link).toBeDefined();
  expect(link!.disabled).toBe(false);
  await act(async () => link!.click());
  expect(chat.linkDevice).toHaveBeenCalledWith(
    "pairing-device",
    "pairing-code",
  );
  await replaceSession("owner-b", 2);
  await act(async () => linking.resolve("linked-account"));
  expect(auth.adoptLinkedAccount).not.toHaveBeenCalled();
});
