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
import { accountDocuments } from "@/lib/api/accountDocuments";
import { diag } from "@/lib/api/diagnostics";
import i18n, { setLanguage } from "@/lib/i18n";
import { publishAppearanceBeforeLink } from "@/lib/linkedAppearance";
import { publishContactsBeforeLink } from "@/lib/linkedContacts";
import { publishDraftsBeforeLink, useDrafts } from "@/store/drafts";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { LinkDeviceDialog } from "./LinkDeviceDialog";

vi.mock("@/lib/api/accountDocuments", async (original) => ({
  ...(await original<typeof import("@/lib/api/accountDocuments")>()),
  accountDocuments: {
    ...(await original<typeof import("@/lib/api/accountDocuments")>())
      .accountDocuments,
    publishLegacyControls: vi.fn().mockResolvedValue(undefined),
  },
}));

vi.mock("@/lib/linkedAppearance", () => ({
  publishAppearanceBeforeLink: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@/lib/linkedContacts", () => ({
  publishContactsBeforeLink: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("@/store/drafts", async (original) => ({
  ...(await original<typeof import("@/store/drafts")>()),
  publishDraftsBeforeLink: vi
    .fn()
    .mockResolvedValue({ imported: 0, unmapped: 0 }),
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
  useDrafts.getState().reset();
  vi.mocked(accountDocuments.publishLegacyControls)
    .mockReset()
    .mockResolvedValue(undefined);
  vi.mocked(publishDraftsBeforeLink)
    .mockReset()
    .mockResolvedValue({ imported: 0, unmapped: 0 });
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
  vi.mocked(publishContactsBeforeLink).mockReset().mockResolvedValue(undefined);
  vi.spyOn(chat, "getAccountLoginSyncStatus").mockResolvedValue({
    account_id: "our-account",
    available_revision: null,
    applied_revision: null,
    devices: [],
  });
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

async function enterPairing(
  code = "0123456789abcdef0123456789abcdef",
  confirm = true,
) {
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
    )!.set!.call(input, code);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  if (confirm) {
    const consent = dialog()!.querySelector<HTMLInputElement>(
      "#link-account-consent",
    );
    if (consent && !consent.checked) await act(async () => consent.click());
  }
}

function pairingPeer(directly_reachable?: boolean) {
  useChat.setState({
    peers: [
      {
        user_id: "pairing-device",
        account_id: "other-account",
        name: "Duncan",
        addr: "127.0.0.1:47479",
        post_office: false,
        discovered_on_lan: false,
        directly_reachable,
      },
    ],
  });
}

function linkButton() {
  return dialog()!.querySelector<HTMLButtonElement>(
    '[data-testid="link-adopt-account"]',
  )!;
}

it("explains which account is kept and requires confirmation before replacing this device's account", async () => {
  pairingPeer(true);
  vi.spyOn(chat, "linkDevice").mockResolvedValue("linked-account");
  await render();
  await open();
  await enterPairing(undefined, false);
  expect(dialog()!.textContent).toContain("Keep this device’s account");
  expect(dialog()!.textContent).toContain(
    "Switch this device to another account",
  );
  const preview = dialog()!.querySelector(
    '[data-testid="link-account-change"]',
  );
  expect(preview?.textContent).toContain("our-account");
  expect(preview?.textContent).toContain("other-account".slice(0, 12));
  expect(preview?.textContent).toContain("Duncan");
  const submit = dialog()!.querySelector<HTMLButtonElement>(
    '[data-testid="link-adopt-account"]',
  )!;
  expect(submit.disabled).toBe(true);
  const consent = dialog()!.querySelector<HTMLInputElement>(
    "#link-account-consent",
  )!;
  await act(async () => consent.click());
  expect(submit.disabled).toBe(false);
  await act(async () => pairingPeer(false));
  expect(submit.disabled).toBe(true);
  await act(async () => pairingPeer(true));
  expect(
    dialog()!.querySelector<HTMLInputElement>("#link-account-consent")!.checked,
  ).toBe(false);
  expect(submit.disabled).toBe(true);
  expect(chat.linkDevice).not.toHaveBeenCalled();
});

it("previews the kept account login separately from its nickname and confirms shared credentials", async () => {
  useAuth.setState({
    user: {
      id: "owner-a",
      username: "SourceLogin",
      display_name: "Shared Nickname",
    } as NonNullable<ReturnType<typeof useAuth.getState>["user"]>,
  });
  pairingPeer(true);
  await render();
  await open();
  const preview = dialog()!.querySelector(
    '[data-testid="link-source-identity"]',
  );
  expect(preview?.textContent).toContain("Login name: SourceLogin");
  expect(preview?.textContent).toContain("Nickname: Shared Nickname");
  await enterPairing(undefined, false);
  const consent = dialog()!.querySelector('label[for="link-account-consent"]');
  expect(consent?.textContent).toContain("login name and password");
  expect(dialog()!.textContent).not.toContain(
    "Your local sign-in and password stay on this device",
  );
  expect(linkButton().disabled).toBe(true);
});

it("requires both apps to upgrade for shared credentials without offering a history fallback", async () => {
  pairingPeer(true);
  vi.spyOn(chat, "linkDevice").mockRejectedValue({
    kind: "link-upgrade-required",
    message: "Account login synchronization is required",
  });
  vi.spyOn(auth, "adoptLinkedAccount").mockResolvedValue(undefined);
  await render();
  await open();
  await enterPairing();
  await act(async () => linkButton().click());
  const feedback = dialog()!.querySelector("#link-device-feedback")!;
  expect(feedback.textContent).toContain("Update Mesh-Talk on both devices");
  expect(feedback.textContent).toContain("login name and password");
  expect(feedback.textContent).not.toContain("Keep history off");
  expect(auth.adoptLinkedAccount).not.toHaveBeenCalled();
});

it("canceled source verification does not publish data or enable synchronization", async () => {
  vi.mocked(chat.startLinking).mockRejectedValueOnce({
    kind: "authentication-canceled",
    message: "Verification canceled",
  });
  await render();
  await open();
  await showCode();
  expect(chat.setDeviceSyncCategory).not.toHaveBeenCalled();
  expect(publishAppearanceBeforeLink).not.toHaveBeenCalled();
  expect(accountDocuments.publishLegacyControls).not.toHaveBeenCalled();
});

it("canceled joining verification does not enable synchronization", async () => {
  pairingPeer(true);
  vi.spyOn(chat, "linkDevice").mockRejectedValueOnce({
    kind: "authentication-canceled",
    message: "Verification canceled",
  });
  await render();
  await open();
  await enterPairing();
  await act(async () => linkButton().click());
  expect(chat.setDeviceSyncCategory).not.toHaveBeenCalled();
});

it("authenticates source sync expansion before publishing snapshots", async () => {
  const pending = deferred<string>();
  vi.mocked(chat.startLinking).mockReturnValueOnce(pending.promise);
  await render();
  await open();
  await showCode();
  expect(chat.startLinking).toHaveBeenCalledWith(true);
  expect(chat.setDeviceSyncCategory).not.toHaveBeenCalled();
  expect(publishAppearanceBeforeLink).not.toHaveBeenCalled();
  await act(async () => pending.resolve("new-code"));
  expect(publishAppearanceBeforeLink).toHaveBeenCalledWith("owner-a");
  expect(
    dialog()
      ?.querySelector('[data-testid="pairing-code"]')
      ?.textContent?.replace(/ /g, ""),
  ).toBe("new-code");
});

it("conceals the authenticated source code until all snapshots are ready", async () => {
  const pending = deferred<void>();
  vi.mocked(publishContactsBeforeLink).mockReturnValue(pending.promise);
  await render();
  await open();
  await showCode();
  expect(chat.startLinking).toHaveBeenCalledWith(true);
  expect(
    dialog()?.querySelector('[data-testid="pairing-code"]') ?? null,
  ).toBeNull();
  await act(async () => pending.resolve());
  expect(publishAppearanceBeforeLink).toHaveBeenCalledWith("owner-a");
  expect(publishContactsBeforeLink).toHaveBeenCalledWith("owner-a");
  expect(publishDraftsBeforeLink).toHaveBeenCalledWith("owner-a");
  expect(vi.mocked(chat.startLinking).mock.invocationCallOrder[0]).toBeLessThan(
    vi.mocked(publishAppearanceBeforeLink).mock.invocationCallOrder[0],
  );
  expect(
    dialog()?.querySelector('[data-testid="pairing-code"]'),
  ).not.toBeNull();
});

it("prepares historical account controls before drafts or a source code", async () => {
  const pending = deferred<void>();
  vi.mocked(accountDocuments.publishLegacyControls).mockReturnValueOnce(
    pending.promise,
  );
  await render();
  await open();
  await showCode();
  expect(accountDocuments.publishLegacyControls).toHaveBeenCalledWith(
    "owner-a",
    "our-account",
  );
  expect(publishDraftsBeforeLink).not.toHaveBeenCalled();
  expect(
    dialog()?.querySelector('[data-testid="pairing-code"]') ?? null,
  ).toBeNull();
  expect(vi.mocked(chat.startLinking).mock.invocationCallOrder[0]).toBeLessThan(
    vi.mocked(accountDocuments.publishLegacyControls).mock
      .invocationCallOrder[0],
  );
  await act(async () => pending.resolve());
  expect(publishDraftsBeforeLink).toHaveBeenCalledWith("owner-a");
  expect(chat.startLinking).toHaveBeenCalledOnce();
});

it("blocks a source code when historical account controls cannot be prepared", async () => {
  vi.mocked(accountDocuments.publishLegacyControls).mockRejectedValueOnce(
    new Error("Could not prepare historical controls"),
  );
  await render();
  await open();
  await showCode();
  expect(
    dialog()?.querySelector('[data-testid="pairing-code"]') ?? null,
  ).toBeNull();
  expect(publishDraftsBeforeLink).not.toHaveBeenCalled();
  expect(chat.stopLinking).toHaveBeenCalledOnce();
  expect(dialog()!.textContent).toContain(
    "Could not prepare historical controls",
  );
});

it("does not import historical controls when source History sharing is off", async () => {
  await render();
  await open();
  await act(async () =>
    dialog()!.querySelector<HTMLInputElement>("#link-sync-all")!.click(),
  );
  await showCode();
  expect(accountDocuments.publishLegacyControls).not.toHaveBeenCalled();
  expect(publishDraftsBeforeLink).not.toHaveBeenCalled();
  expect(chat.startLinking).toHaveBeenCalledOnce();
});

it.each(["session", "account"])(
  "does not publish a source code after its %s changes during control preparation",
  async (scope) => {
    const pending = deferred<void>();
    vi.mocked(accountDocuments.publishLegacyControls).mockReturnValueOnce(
      pending.promise,
    );
    await render();
    await open();
    await showCode();
    if (scope === "session") await replaceSession("owner-b", 2);
    else
      await act(async () =>
        useChat.setState({ myAccountId: "replacement-account" }),
      );
    await act(async () => pending.resolve());
    expect(publishDraftsBeforeLink).not.toHaveBeenCalled();
    expect(
      dialog()?.querySelector('[data-testid="pairing-code"]') ?? null,
    ).toBeNull();
  },
);

it("does not show a pairing code when source preference preparation fails", async () => {
  vi.mocked(publishContactsBeforeLink).mockRejectedValue(
    new Error("Could not save contacts"),
  );
  await render();
  await open();
  await showCode();
  expect(
    dialog()?.querySelector('[data-testid="pairing-code"]') ?? null,
  ).toBeNull();
  expect(chat.stopLinking).toHaveBeenCalledOnce();
  expect(dialog()!.textContent).toContain("Could not save contacts");
});

it("enables all categories on the joining device without publishing its local preferences", async () => {
  pairingPeer(true);
  vi.spyOn(chat, "linkDevice").mockRejectedValue({
    kind: "link-rejected",
    message: "closed",
  });
  await render();
  await open();
  await enterPairing();
  await act(async () => linkButton().click());
  expect(chat.linkDevice).toHaveBeenCalledWith(
    "pairing-device",
    "0123456789abcdef0123456789abcdef",
    true,
  );
  expect(chat.setDeviceSyncCategory).not.toHaveBeenCalled();
  expect(publishAppearanceBeforeLink).not.toHaveBeenCalled();
  expect(publishContactsBeforeLink).not.toHaveBeenCalled();
  expect(accountDocuments.publishLegacyControls).not.toHaveBeenCalled();
});

it("retains explicit category choices when sync all is turned off", async () => {
  await render();
  await open();
  await act(async () =>
    dialog()!.querySelector<HTMLInputElement>("#link-sync-all")!.click(),
  );
  await showCode();
  expect(chat.setDeviceSyncCategory).not.toHaveBeenCalled();
  expect(chat.startLinking).toHaveBeenCalled();
});

it("excludes post-office-only devices and prevents a link attempt", async () => {
  pairingPeer(false);
  vi.spyOn(chat, "linkDevice").mockResolvedValue("linked-account");
  await render();
  await open();
  await enterPairing();
  expect(dialog()!.querySelector("option[value=pairing-device]")).toBeNull();
  expect(
    dialog()!.querySelector("#link-device-route-hint")!.textContent,
  ).toContain("cannot be selected");
  expect(linkButton().disabled).toBe(true);
  await act(async () => linkButton().click());
  expect(chat.linkDevice).not.toHaveBeenCalled();
});

it("withdraws a selected device when its direct route disappears and restores it after discovery", async () => {
  pairingPeer(true);
  vi.spyOn(chat, "linkDevice").mockResolvedValue("linked-account");
  await render();
  await open();
  await enterPairing();
  expect(linkButton().disabled).toBe(false);
  await act(async () => pairingPeer(false));
  expect(dialog()!.querySelector("option[value=pairing-device]")).toBeNull();
  expect(
    dialog()!.querySelector<HTMLSelectElement>("#link-device-peer")!.value,
  ).toBe("");
  expect(linkButton().disabled).toBe(true);
  await act(async () => linkButton().click());
  expect(chat.linkDevice).not.toHaveBeenCalled();
  await act(async () => pairingPeer(true));
  expect(
    dialog()!.querySelector("option[value=pairing-device]"),
  ).not.toBeNull();
  await enterPairing();
  expect(linkButton().disabled).toBe(false);
});

it("disables linking when the selected device disappears from discovery", async () => {
  pairingPeer(true);
  await render();
  await open();
  await enterPairing();
  await act(async () => useChat.setState({ peers: [] }));
  expect(linkButton().disabled).toBe(true);
});

it.each([true, undefined])(
  "allows a routed or older-backend device without inventing a relay-only restriction (%s)",
  async (direct) => {
    pairingPeer(direct);
    await render();
    await open();
    await enterPairing();
    expect(
      dialog()!.querySelector("option[value=pairing-device]")!.textContent,
    ).not.toContain("via post office");
    expect(linkButton().disabled).toBe(false);
  },
);

it("keeps actionable connection errors next to Link and puts raw diagnostics in a disclosure", async () => {
  pairingPeer(true);
  vi.spyOn(chat, "linkDevice").mockRejectedValue({
    kind: "link-connection",
    message: "sync transport error: os error 60",
  });
  await render();
  await open();
  await enterPairing("0123 4567 89ab cdef 0123 4567 89ab cdef");
  await act(async () => linkButton().click());
  expect(chat.linkDevice).toHaveBeenCalledWith(
    "pairing-device",
    "0123456789abcdef0123456789abcdef",
    true,
  );
  const error = dialog()!.querySelector("#link-device-feedback")!;
  expect(error.getAttribute("role")).toBe("alert");
  expect(error.textContent).toContain("Couldn’t connect to this device");
  expect(
    error.closest("section")!.querySelector("#link-device-code"),
  ).not.toBeNull();
  expect(error.querySelector("details")!.open).toBe(false);
  expect(error.querySelector("details")!.textContent).toContain("os error 60");
});

it("announces a pending connection and disables mutable pairing fields", async () => {
  pairingPeer(true);
  const pending = deferred<string>();
  vi.spyOn(chat, "linkDevice").mockReturnValue(pending.promise);
  await render();
  await open();
  await enterPairing();
  await act(async () => linkButton().click());
  expect(dialog()!.textContent).toContain("Connecting…");
  expect(
    dialog()!.querySelector<HTMLInputElement>("#link-device-code")!.disabled,
  ).toBe(true);
  expect(
    dialog()!.querySelector<HTMLSelectElement>("#link-device-peer")!.disabled,
  ).toBe(true);
  await act(async () =>
    pending.reject({
      kind: "link-timeout",
      message: "pairing response timed out",
    }),
  );
  expect(
    dialog()!.querySelector("#link-device-feedback")!.textContent,
  ).toContain("didn’t answer in time");
  expect(linkButton().disabled).toBe(false);
});

it("requests discovery from the pairing form and reports refresh failures independently", async () => {
  const rescan = vi
    .spyOn(diag, "rescan")
    .mockRejectedValue(new Error("discovery unavailable"));
  await render();
  await open();
  const refresh = dialog()!.querySelector<HTMLButtonElement>(
    '[data-testid="link-refresh-devices"]',
  )!;
  expect(refresh).not.toBeNull();
  await act(async () => refresh.click());
  expect(rescan).toHaveBeenCalledOnce();
  expect(dialog()!.textContent).toContain("discovery unavailable");
  expect(dialog()!.querySelector("#link-device-feedback")).toBeNull();
});

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
  expect(publishAppearanceBeforeLink).not.toHaveBeenCalled();
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

it("does not display a prepared pairing code after its owner retires", async () => {
  const oldAppearance = deferred<void>();
  vi.mocked(publishAppearanceBeforeLink).mockReturnValueOnce(
    oldAppearance.promise,
  );
  await render();
  await open();
  await showCode();
  expect(
    dialog()?.querySelector('[data-testid="pairing-code"]') ?? null,
  ).toBeNull();
  await replaceSession("owner-b", 2);
  await act(async () => oldAppearance.resolve());
  expect(
    dialog()?.querySelector('[data-testid="pairing-code"]') ?? null,
  ).toBeNull();
  expect(
    dialog()?.querySelector('[data-testid="pairing-code"]') ?? null,
  ).toBeNull();
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
  await act(async () =>
    dialog()!.querySelector<HTMLInputElement>("#link-account-consent")!.click(),
  );
  const link = linkButton();
  expect(link).toBeDefined();
  expect(link!.disabled).toBe(false);
  await act(async () => link!.click());
  expect(chat.linkDevice).toHaveBeenCalledWith(
    "pairing-device",
    "pairing-code",
    true,
  );
  await replaceSession("owner-b", 2);
  await act(async () => linking.resolve("linked-account"));
  expect(auth.adoptLinkedAccount).not.toHaveBeenCalled();
});

it("distinguishes a pending account login from message history already being up to date", async () => {
  vi.spyOn(chat, "getAccountLoginSyncStatus").mockResolvedValue({
    account_id: "our-account",
    available_revision: 7,
    applied_revision: 6,
    devices: [
      {
        device_id: "new-device",
        phase: "retry_scheduled",
        revision: 6,
        failure: "timeout",
        attempts: 2,
        retry_delay_ms: 5000,
      },
    ],
  });
  await render();
  await open();
  const login = dialog()!.querySelector(
    '[data-testid="account-login-sync-status"]',
  )!;
  expect(login.textContent).toContain("Applying the shared account login");
  expect(login.textContent).toContain("Keep both apps open");
  expect(login.textContent).not.toContain(
    "This device uses the shared login name and password",
  );
  expect(dialog()!.textContent).toContain("Up to date");
});

it("does not claim up to date while account documents are still catching up", async () => {
  vi.mocked(chat.getDeviceSyncStatus).mockResolvedValue([
    {
      ...status("new-device"),
      document_phase: "catching_up",
      document_error: null,
      document_requires_upgrade: false,
    },
  ]);
  await render();
  await open();
  const device = Array.from(dialog()!.querySelectorAll("span.font-mono")).find(
    (element) => element.textContent === "new-devi",
  )?.parentElement;
  expect(device?.textContent).toContain("Catching up");
  expect(device?.textContent).not.toContain("Up to date");
});

it("keeps account document progress visible while file bytes are waiting", async () => {
  vi.mocked(chat.getDeviceSyncStatus).mockResolvedValue([
    {
      ...status("new-device"),
      phase: "waiting_for_files",
      pending_attachments: 9,
      document_phase: "catching_up",
    },
  ]);
  await render();
  await open();
  const device = Array.from(dialog()!.querySelectorAll("span.font-mono")).find(
    (element) => element.textContent === "new-devi",
  )?.parentElement;
  expect(device?.textContent).toContain("Catching up");
  expect(device?.textContent).toContain("9 attachments pending");
  expect(device?.textContent).not.toContain("Up to date");
});

it("explains waiting file bytes without claiming messages or settings are blocked", async () => {
  vi.mocked(chat.getDeviceSyncStatus).mockResolvedValue([
    {
      ...status("new-device"),
      phase: "waiting_for_files",
      pending_attachments: 9,
      document_phase: "up_to_date",
    },
  ]);
  await render();
  await open();
  const device = Array.from(dialog()!.querySelectorAll("span.font-mono")).find(
    (element) => element.textContent === "new-devi",
  )?.parentElement;
  expect(device?.textContent).toContain("Waiting for files");
  expect(device?.textContent).toContain("Messages and settings can still sync");
  expect(device?.textContent).toContain("9 attachments pending");
  expect(device?.textContent).not.toContain("Up to date");
  expect(device?.textContent).not.toContain("Sync error");
});

it.each<[Partial<DeviceSyncStatus>, string, string]>([
  [
    { document_phase: "error", document_error: "internal transfer failure" },
    "Sync error",
    "documentsError",
  ],
  [
    { document_phase: "up_to_date", document_requires_upgrade: true },
    "Sync error",
    "documentsUpgrade",
  ],
  [
    { document_phase: "disabled", document_error: "source_sync_off" },
    "Sync off",
    "documentsDisabled",
  ],
])(
  "prioritizes document readiness over waiting files for %j",
  async (change, caption, hint) => {
    vi.mocked(chat.getDeviceSyncStatus).mockResolvedValue([
      {
        ...status("new-device"),
        phase: "waiting_for_files",
        pending_attachments: 9,
        ...change,
      },
    ]);
    await render();
    await open();
    const device = Array.from(
      dialog()!.querySelectorAll("span.font-mono"),
    ).find((element) => element.textContent === "new-devi")?.parentElement;
    expect(device?.textContent).toContain(caption);
    expect(device?.textContent).toContain(i18n.t(`linkDevice.${hint}`));
    expect(device?.textContent).toContain("9 attachments pending");
    expect(device?.textContent).not.toContain("Waiting for files");
    expect(device?.textContent).not.toContain("Up to date");
  },
);

it("keeps waiting files readable when older status responses omit document fields", async () => {
  vi.mocked(chat.getDeviceSyncStatus).mockResolvedValue([
    {
      ...status("new-device"),
      phase: "waiting_for_files",
      pending_attachments: 9,
    },
  ]);
  await render();
  await open();
  expect(dialog()!.textContent).toContain("Waiting for files");
  expect(dialog()!.textContent).toContain(
    "Messages and settings can still sync",
  );
  expect(dialog()!.textContent).not.toContain("Up to date");
});

it.each<[Partial<DeviceSyncStatus>, string, string]>([
  [
    {
      document_phase: "error",
      document_error: "internal document checksum failure",
    },
    "Sync error",
    "documentsError",
  ],
  [
    {
      document_phase: "error",
      document_error: "UNTRUSTED_DOCUMENT_ERROR_63",
      document_requires_upgrade: true,
    },
    "Sync error",
    "documentsUpgrade",
  ],
  [
    { document_phase: "disabled", document_error: "source_sync_off" },
    "Sync off",
    "documentsDisabled",
  ],
])(
  "shows document readiness and a localized explanation for %j",
  async (change, caption, hint) => {
    vi.mocked(chat.getDeviceSyncStatus).mockResolvedValue([
      { ...status("new-device"), ...change },
    ]);
    await render();
    await open();
    const device = Array.from(
      dialog()!.querySelectorAll("span.font-mono"),
    ).find((element) => element.textContent === "new-devi")?.parentElement;
    expect(device?.textContent).toContain(caption);
    expect(device?.textContent).toContain(i18n.t(`linkDevice.${hint}`));
    expect(device?.textContent).not.toContain(change.document_error);
  },
);

it("retains ordinary offline status while documents have not settled", async () => {
  vi.mocked(chat.getDeviceSyncStatus).mockResolvedValue([
    {
      ...status("new-device"),
      phase: "offline",
      document_phase: "catching_up",
    },
  ]);
  await render();
  await open();
  const device = Array.from(dialog()!.querySelectorAll("span.font-mono")).find(
    (element) => element.textContent === "new-devi",
  )?.parentElement;
  expect(device?.textContent).toContain("Offline");
  expect(device?.textContent).not.toContain("Catching up");
});

it("shows applied shared sign-in only when its available and applied revisions match", async () => {
  vi.spyOn(chat, "getAccountLoginSyncStatus").mockResolvedValue({
    account_id: "our-account",
    available_revision: 7,
    applied_revision: 7,
    devices: [],
  });
  await render();
  await open();
  expect(
    dialog()!.querySelector('[data-testid="account-login-sync-status"]')
      ?.textContent,
  ).toContain("This device uses the shared login name and password");
});

it("requires an upgrade for an unsupported login source without claiming history proves sign-in completion", async () => {
  vi.spyOn(chat, "getAccountLoginSyncStatus").mockResolvedValue({
    account_id: "our-account",
    available_revision: null,
    applied_revision: null,
    devices: [
      {
        device_id: "new-device",
        phase: "unsupported",
        revision: 0,
        failure: "unsupported_protocol",
        attempts: 1,
        retry_delay_ms: 60000,
      },
    ],
  });
  await render();
  await open();
  expect(
    dialog()!.querySelector('[data-testid="account-login-sync-status"]')
      ?.textContent,
  ).toContain("Update Mesh-Talk on both devices");
});

it("discards an old account-login status response after the authenticated session changes", async () => {
  const previous =
    deferred<Awaited<ReturnType<typeof chat.getAccountLoginSyncStatus>>>();
  vi.mocked(chat.getAccountLoginSyncStatus)
    .mockReturnValueOnce(previous.promise)
    .mockResolvedValue({
      account_id: "our-account",
      available_revision: 9,
      applied_revision: 9,
      devices: [],
    });
  await render();
  await open();
  await replaceSession("owner-b", 2);
  expect(
    dialog()!.querySelector('[data-testid="account-login-sync-status"]')
      ?.textContent,
  ).toContain("This device uses the shared login name and password");
  await act(async () =>
    previous.resolve({
      account_id: "our-account",
      available_revision: 7,
      applied_revision: null,
      devices: [],
    }),
  );
  expect(
    dialog()!.querySelector('[data-testid="account-login-sync-status"]')
      ?.textContent,
  ).toContain("This device uses the shared login name and password");
});

it("keeps unavailable login progress actionable when an older host returns no status record", async () => {
  vi.mocked(chat.getAccountLoginSyncStatus).mockResolvedValue(null as never);
  await render();
  await open();
  expect(
    dialog()!.querySelector('[data-testid="account-login-sync-status"]')
      ?.textContent,
  ).toContain("Couldn’t check account sign-in");
});
