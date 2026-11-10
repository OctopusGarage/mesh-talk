// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  open: vi.fn(),
  state: { generation: 0 },
  change: null as null | ((open: boolean) => void),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: mocks.open }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@/store/auth", () => ({
  useAuth: (select: (state: typeof mocks.state) => unknown) =>
    select(mocks.state),
}));
vi.mock("@/features/recovery/GoogleDriveProgress", () => ({
  GoogleDriveProgress: () => null,
}));
vi.mock("@/features/recovery/GoogleDriveConnection", () => ({
  GoogleDriveConnection: ({
    onConnected,
    onDisconnected,
  }: {
    onConnected: () => void;
    onDisconnected: () => void;
  }) =>
    createElement(
      "div",
      null,
      createElement("button", { onClick: onConnected }, "connect"),
      createElement("button", { onClick: onDisconnected }, "disconnect"),
    ),
}));
vi.mock("@/components/ui/dialog", () => {
  const Wrap = ({ children }: { children: ReactNode }) =>
    createElement("div", null, children);
  return {
    Dialog: ({
      children,
      onOpenChange,
    }: {
      children: ReactNode;
      onOpenChange: (open: boolean) => void;
    }) => {
      mocks.change = onOpenChange;
      return createElement("div", null, children);
    },
    DialogContent: Wrap,
    DialogHeader: Wrap,
    DialogTitle: Wrap,
    DialogDescription: Wrap,
    DialogTrigger: Wrap,
  };
});
import { RestoreBackup } from "./RestoreBackup";
const preview = {
  account_id: "verified-account",
  created_at_ms: 1,
  messages: 12,
  channels: 2,
  file_cards: 3,
  media_files: 2,
  missing_media: 1,
  favorites: 4,
  favorite_files: 2,
  missing_favorite_files: 1,
};
const archives = [
  { id: "first", name: "first.mtbackup", size: "1048576", checksum: "a" },
  { id: "second", name: "second.mtbackup", size: "2097152", checksum: "b" },
];
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
let root: Root;
let host: HTMLDivElement;
let onRestored: ReturnType<typeof vi.fn<(username: string) => void>>;
let onStartFresh: ReturnType<typeof vi.fn<() => void>>;
const render = () =>
  act(async () =>
    root.render(createElement(RestoreBackup, { onRestored, onStartFresh })),
  );
const byId = <T extends HTMLElement>(id: string) =>
  host.querySelector<T>(`[data-testid="${id}"]`)!;
const button = (label: string) =>
  [...host.querySelectorAll("button")].find(
    (item) => item.textContent === label,
  )!;
const click = (label: string) => act(async () => button(label).click());
async function input(id: string, value: string) {
  await act(async () => {
    const element = byId<HTMLInputElement>(id);
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function select(id: string, value: string) {
  await act(async () => {
    const element = host.querySelector<HTMLSelectElement>(`#${id}`)!;
    element.value = value;
    element.dispatchEvent(new Event("change", { bubbles: true }));
  });
}
async function drive() {
  await select("backup-source", "drive");
  await click("connect");
}
async function inspectLocal() {
  await click("recovery.chooseFile");
  await input("restore-key", "archive-secret");
  await act(async () => byId<HTMLButtonElement>("restore-inspect").click());
}
async function credentials() {
  await input("restore-username", " alice ");
  await input("restore-password", "new-password");
}
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.resetAllMocks();
  mocks.state.generation = 0;
  mocks.open.mockResolvedValue("/tmp/archive.mtbackup");
  mocks.invoke.mockImplementation(async (command: string) => {
    if (command === "google_drive_list") return archives;
    if (command.endsWith("inspect") || command === "inspect_backup")
      return preview;
    if (command === "restore_backup" || command === "google_drive_restore")
      return { username: "alice", device_id: "new-device", preview };
  });
  onRestored = vi.fn();
  onStartFresh = vi.fn();
  host = document.createElement("div");
  root = createRoot(host);
  await render();
  await act(async () => mocks.change?.(true));
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
it("lists Drive archives and downloads the chosen archive only after Continue", async () => {
  await drive();
  expect(host.textContent).toContain("first.mtbackup (1.0 MB)");
  expect(mocks.invoke).not.toHaveBeenCalledWith(
    "google_drive_download",
    expect.anything(),
  );
  await select("drive-backup", "second");
  await input("restore-key", "drive-secret");
  await act(async () => byId<HTMLButtonElement>("restore-inspect").click());
  expect(mocks.invoke).toHaveBeenCalledWith("google_drive_download", {
    connectionId: expect.any(String),
    fileId: "second",
  });
  expect(mocks.invoke).toHaveBeenCalledWith("google_drive_inspect", {
    connectionId: expect.any(String),
    recoveryKey: "drive-secret",
  });
  expect(byId("restore-preview").textContent).toContain("verified-account");
  expect(onRestored).not.toHaveBeenCalled();
  await credentials();
  await act(async () => byId<HTMLButtonElement>("restore-commit").click());
  expect(mocks.invoke).toHaveBeenCalledWith("google_drive_restore", {
    connectionId: expect.any(String),
    recoveryKey: "drive-secret",
    username: "alice",
    newPassword: "new-password",
  });
  expect(onRestored).toHaveBeenCalledWith("alice");
  expect(host.textContent).toContain("new-device");
  await click("recovery.signInNext");
});
it("retries a failed Drive list and explains an empty account", async () => {
  mocks.invoke
    .mockRejectedValueOnce(new Error("list unavailable"))
    .mockResolvedValueOnce([]);
  await drive();
  expect(host.textContent).toContain("list unavailable");
  await click("drive.load");
  expect(host.textContent).toContain("drive.empty");
  expect(byId<HTMLButtonElement>("restore-inspect").disabled).toBe(true);
});
it("keeps the downloaded archive for a wrong-key retry and resets it for another selection", async () => {
  await drive();
  await input("restore-key", "wrong");
  mocks.invoke
    .mockResolvedValueOnce(undefined)
    .mockRejectedValueOnce(new Error("wrong recovery key"));
  await act(async () => byId<HTMLButtonElement>("restore-inspect").click());
  expect(host.textContent).toContain("wrong recovery key");
  await input("restore-key", "correct");
  await act(async () => byId<HTMLButtonElement>("restore-inspect").click());
  expect(
    mocks.invoke.mock.calls.filter(
      ([command]) => command === "google_drive_download",
    ),
  ).toHaveLength(1);
  expect(byId("restore-preview")).not.toBeNull();
  await select("drive-backup", "second");
  expect(byId<HTMLInputElement>("restore-key").value).toBe("");
  expect(byId("restore-preview")).toBeNull();
  await input("restore-key", "second-secret");
  await act(async () => byId<HTMLButtonElement>("restore-inspect").click());
  expect(
    mocks.invoke.mock.calls.filter(
      ([command]) => command === "google_drive_download",
    ),
  ).toHaveLength(2);
});
it("never inspects after a failed download and releases retry controls", async () => {
  await drive();
  await input("restore-key", "key");
  mocks.invoke.mockRejectedValueOnce(new Error("download interrupted"));
  await act(async () => byId<HTMLButtonElement>("restore-inspect").click());
  expect(host.textContent).toContain("download interrupted");
  expect(
    mocks.invoke.mock.calls.some(
      ([command]) => command === "google_drive_inspect",
    ),
  ).toBe(false);
  expect(byId<HTMLButtonElement>("restore-inspect").disabled).toBe(false);
});
it("cancels a download and ignores its late completion before accepting another source", async () => {
  await drive();
  await input("restore-key", "key");
  const pending = deferred<void>();
  mocks.invoke.mockReturnValueOnce(pending.promise);
  await act(async () => {
    byId<HTMLButtonElement>("restore-inspect").click();
    byId<HTMLButtonElement>("restore-inspect").click();
  });
  expect(
    mocks.invoke.mock.calls.filter(
      ([command]) => command === "google_drive_download",
    ),
  ).toHaveLength(1);
  await click("drive.cancel");
  expect(mocks.invoke).toHaveBeenCalledWith("google_drive_disconnect", {
    connectionId: expect.any(String),
  });
  await select("backup-source", "local");
  await act(async () => pending.resolve());
  expect(
    mocks.invoke.mock.calls.some(
      ([command]) => command === "google_drive_inspect",
    ),
  ).toBe(false);
  await inspectLocal();
  expect(byId("restore-preview")).not.toBeNull();
});
it("forgets Drive selection and private key after disconnect", async () => {
  await drive();
  await input("restore-key", "private-key");
  await click("disconnect");
  expect(byId("restore-key")).toBeNull();
  expect(host.querySelector("#drive-backup")).toBeNull();
});
it("reports picker failure, admits one picker, and ignores an old owner's chosen path", async () => {
  mocks.open.mockRejectedValueOnce(new Error("picker unavailable"));
  await click("recovery.chooseFile");
  expect(host.textContent).toContain("picker unavailable");
  const pending = deferred<string>();
  mocks.open.mockReturnValueOnce(pending.promise);
  await act(async () => {
    button("recovery.chooseFile").click();
    button("recovery.chooseFile").click();
  });
  expect(mocks.open).toHaveBeenCalledTimes(2);
  mocks.state.generation++;
  await render();
  await act(async () => pending.resolve("/tmp/retired.mtbackup"));
  expect(host.textContent).not.toContain("retired.mtbackup");
  expect(byId("restore-key")).toBeNull();
});
it("requires verified local preview and valid credentials before restoring, then permits retry", async () => {
  await inspectLocal();
  expect(mocks.invoke).toHaveBeenCalledWith("inspect_backup", {
    path: "/tmp/archive.mtbackup",
    recoveryKey: "archive-secret",
  });
  expect(byId<HTMLButtonElement>("restore-commit").disabled).toBe(true);
  await credentials();
  mocks.invoke.mockRejectedValueOnce(new Error("username exists"));
  await act(async () => byId<HTMLButtonElement>("restore-commit").click());
  expect(host.textContent).toContain("username exists");
  expect(byId<HTMLInputElement>("restore-password").value).toBe("new-password");
  await act(async () => byId<HTMLButtonElement>("restore-commit").click());
  expect(mocks.invoke).toHaveBeenCalledWith("restore_backup", {
    path: "/tmp/archive.mtbackup",
    recoveryKey: "archive-secret",
    username: "alice",
    newPassword: "new-password",
  });
  expect(onRestored).toHaveBeenCalledOnce();
  expect(byId("restore-password")).toBeNull();
  await act(async () => mocks.change?.(false));
  await act(async () => mocks.change?.(true));
  expect(byId<HTMLInputElement>("restore-key").value).toBe("");
  expect(byId("restore-preview")).toBeNull();
});
it("does not publish a restore completed for a retired auth generation", async () => {
  await inspectLocal();
  await credentials();
  const pending = deferred<unknown>();
  mocks.invoke.mockReturnValueOnce(pending.promise);
  await act(async () => {
    byId<HTMLButtonElement>("restore-commit").click();
    byId<HTMLButtonElement>("restore-commit").click();
  });
  expect(
    mocks.invoke.mock.calls.filter(([command]) => command === "restore_backup"),
  ).toHaveLength(1);
  mocks.state.generation++;
  await render();
  await act(async () =>
    pending.resolve({ username: "old-owner", device_id: "retired", preview }),
  );
  expect(onRestored).not.toHaveBeenCalled();
  expect(host.textContent).not.toContain("retired");
});
it("lets a compromised-device user start fresh and clears restore secrets", async () => {
  await inspectLocal();
  await credentials();
  await act(async () =>
    host.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click(),
  );
  expect(host.textContent).toContain("recovery.compromiseWarning");
  expect(byId("restore-commit")).toBeNull();
  await click("recovery.startFresh");
  expect(onStartFresh).toHaveBeenCalledOnce();
  expect(onRestored).not.toHaveBeenCalled();
  await act(async () => mocks.change?.(true));
  expect(byId<HTMLInputElement>("restore-key").value).toBe("");
  expect(byId("restore-preview")).toBeNull();
});
it("ignores a Drive list response after the owner changes", async () => {
  const pending = deferred<unknown>();
  mocks.invoke.mockReturnValueOnce(pending.promise);
  await drive();
  mocks.state.generation++;
  await render();
  await act(async () => pending.resolve(archives));
  expect(host.querySelector("#drive-backup")).toBeNull();
});
it.each(["resolve", "reject"] as const)(
  "ignores a late verification %s after the restore view unmounts",
  async (outcome) => {
    await click("recovery.chooseFile");
    await input("restore-key", "key");
    const pending = deferred<unknown>();
    mocks.invoke.mockReturnValueOnce(pending.promise);
    await act(async () => byId<HTMLButtonElement>("restore-inspect").click());
    await act(async () => root.unmount());
    await act(async () =>
      outcome === "resolve"
        ? pending.resolve(preview)
        : pending.reject(new Error("old verification error")),
    );
    expect(onRestored).not.toHaveBeenCalled();
    expect(host.textContent).toBe("");
    root = createRoot(host);
  },
);
it("keeps an in-flight local restore owned when a close request arrives", async () => {
  await inspectLocal();
  await credentials();
  const pending = deferred<unknown>();
  mocks.invoke.mockReturnValueOnce(pending.promise);
  await act(async () => byId<HTMLButtonElement>("restore-commit").click());
  await act(async () => mocks.change?.(false));
  expect(byId<HTMLInputElement>("restore-key").value).toBe("archive-secret");
  expect(byId<HTMLButtonElement>("restore-commit").disabled).toBe(true);
  await act(async () =>
    pending.resolve({
      username: "alice",
      device_id: "current-device",
      preview,
    }),
  );
  expect(onRestored).toHaveBeenCalledWith("alice");
});
it("preserves a chosen archive when the file picker is cancelled", async () => {
  await click("recovery.chooseFile");
  await input("restore-key", "secret");
  mocks.open.mockResolvedValueOnce(null);
  await click("archive.mtbackup");
  expect(byId<HTMLInputElement>("restore-key").value).toBe("secret");
  expect(button("archive.mtbackup")).toBeDefined();
});
it.each(["picker", "list", "restore"] as const)(
  "ignores a stale %s failure after its auth owner changes",
  async (operation) => {
    const pending = deferred<never>();
    if (operation === "picker") {
      mocks.open.mockReturnValueOnce(pending.promise);
      await click("recovery.chooseFile");
    } else if (operation === "list") {
      mocks.invoke.mockReturnValueOnce(pending.promise);
      await drive();
    } else {
      await inspectLocal();
      await credentials();
      mocks.invoke.mockReturnValueOnce(pending.promise);
      await act(async () => byId<HTMLButtonElement>("restore-commit").click());
    }
    mocks.state.generation++;
    await render();
    await act(async () =>
      pending.reject(new Error("retired operation failure")),
    );
    expect(host.textContent).not.toContain("retired operation failure");
    expect(onRestored).not.toHaveBeenCalled();
  },
);
