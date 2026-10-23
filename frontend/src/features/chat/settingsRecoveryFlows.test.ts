// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({
  policy: vi.fn(),
  global: vi.fn(),
  contact: vi.fn(),
  prepare: vi.fn(),
  status: vi.fn(),
  create: vi.fn(),
  save: vi.fn(),
  state: { generation: 0, runEpoch: 0, identityEpoch: 0, ready: true },
  dialog: { change: null as null | ((open: boolean) => void) },
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@/lib/api", () => ({
  chat: {
    attachmentReceivePolicy: mocks.policy,
    setAttachmentGlobalAuto: mocks.global,
    setAttachmentContactAuto: mocks.contact,
  },
  recovery: {
    prepareKey: mocks.prepare,
    status: mocks.status,
    create: mocks.create,
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: mocks.save }));
vi.mock("@/store/auth", () => ({
  useAuth: (select: (state: typeof mocks.state) => unknown) =>
    select(mocks.state),
}));
vi.mock("@/store/chat", () => ({
  useChat: (select: (state: typeof mocks.state) => unknown) =>
    select(mocks.state),
  captureChatOwnership: () => {
    const snapshot = { ...mocks.state };
    return {
      current: () =>
        mocks.state.ready &&
        Object.keys(snapshot).every(
          (key) =>
            snapshot[key as keyof typeof snapshot] ===
            mocks.state[key as keyof typeof snapshot],
        ),
    };
  },
}));
vi.mock("@/store/ownership", () => ({
  captureRuntimeOwner: () => {
    const snapshot = { ...mocks.state };
    return {
      current: () =>
        (Object.keys(snapshot) as (keyof typeof snapshot)[])
          .filter((key) => key !== "ready")
          .every((key) => snapshot[key] === mocks.state[key]),
    };
  },
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
      mocks.dialog.change = onOpenChange;
      return createElement("div", null, children);
    },
    DialogContent: Wrap,
    DialogHeader: Wrap,
    DialogTitle: Wrap,
    DialogDescription: Wrap,
    DialogTrigger: Wrap,
  };
});
vi.mock("@/components/ui/popover", () => {
  const Wrap = ({ children }: { children: ReactNode }) =>
    createElement("div", null, children);
  return { Popover: Wrap, PopoverContent: Wrap, PopoverTrigger: Wrap };
});
import { AttachmentReceiveSettings } from "./AttachmentReceiveSettings";
import {
  DataRecoverySettings,
  BackupStatusSummary,
} from "./DataRecoverySettings";
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.resetAllMocks();
  mocks.state = { generation: 0, runEpoch: 0, identityEpoch: 0, ready: true };
  mocks.policy.mockResolvedValue({ global_auto: false, contact_auto: null });
  mocks.prepare.mockResolvedValue("aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh");
  mocks.status.mockResolvedValue(null);
  host = document.createElement("div");
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
const render = async (component: ReactNode) => {
  await act(async () => root.render(component));
};
const select = () => host.querySelector("select")!;
const exportButton = () =>
  host.querySelector<HTMLButtonElement>('[data-testid="recovery-export"]')!;
async function openBackup() {
  await render(createElement(DataRecoverySettings));
  await act(async () => mocks.dialog.change?.(true));
  const input = host.querySelector<HTMLInputElement>("input")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, "bbbb-gggg");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
it("keeps receive policy unknown while loading and allows retry after failure", async () => {
  const pending = deferred<unknown>();
  mocks.policy.mockReturnValueOnce(pending.promise);
  await render(createElement(AttachmentReceiveSettings));
  expect(select().value).toBe("");
  expect(select().disabled).toBe(true);
  await act(async () => pending.reject(new Error("offline")));
  expect(select().value).toBe("");
  expect(select().disabled).toBe(true);
  const retry = Array.from(host.querySelectorAll("button")).find(
    (button) => button.textContent === "contactVisibility.retry",
  );
  expect(retry).toBeDefined();
  await act(async () => retry!.click());
  expect(select().value).toBe("ask");
  expect(select().disabled).toBe(false);
});
it("ignores contact save completions across an account ABA transition", async () => {
  const pending = deferred<void>();
  mocks.contact.mockReturnValueOnce(pending.promise);
  await render(createElement(AttachmentReceiveSettings, { account: "alice" }));
  await act(async () => {
    select().value = "auto";
    select().dispatchEvent(new Event("change", { bubbles: true }));
  });
  await render(createElement(AttachmentReceiveSettings, { account: "bob" }));
  await render(createElement(AttachmentReceiveSettings, { account: "alice" }));
  await act(async () => pending.resolve());
  expect(select().value).toBe("inherit");
});
it("reports native picker rejection and releases the busy state", async () => {
  mocks.save.mockRejectedValue(new Error("picker unavailable"));
  await openBackup();
  await act(async () => exportButton().click());
  expect(host.textContent).toContain("picker unavailable");
  expect(exportButton().disabled).toBe(false);
});
it("admits one picker synchronously and does not export after ownership changes", async () => {
  const pending = deferred<string>();
  mocks.save.mockReturnValue(pending.promise);
  await openBackup();
  await act(async () => {
    exportButton().click();
    exportButton().click();
  });
  expect(mocks.save).toHaveBeenCalledTimes(1);
  mocks.state.generation++;
  await act(async () => pending.resolve("/tmp/stale.mtbackup"));
  expect(mocks.create).not.toHaveBeenCalled();
});
it("reads backup status on the history page without preparing a key", async () => {
  await render(createElement(BackupStatusSummary));
  expect(mocks.status).toHaveBeenCalledTimes(1);
  expect(mocks.prepare).not.toHaveBeenCalled();
  expect(host.textContent).toContain("recovery.noBackup");
  expect(host.textContent).toContain("recovery.restoreHint");
});

it("does not admit a stale backup export after the settings view closes and reopens", async () => {
  const pending = deferred<string>();
  mocks.save.mockReturnValue(pending.promise);
  await openBackup();
  await act(async () => exportButton().click());
  await render(createElement(DataRecoverySettings, { enabled: false }));
  await render(createElement(DataRecoverySettings, { enabled: true }));
  await act(async () => pending.resolve("/tmp/stale.mtbackup"));
  expect(mocks.create).not.toHaveBeenCalled();
  expect(host.querySelector('[data-testid="recovery-export-key"]')).toBeNull();
});

it("ignores a backup key prepared by an earlier owner even after an owner ABA transition", async () => {
  const pending = deferred<string>();
  mocks.prepare.mockReturnValueOnce(pending.promise);
  await render(createElement(DataRecoverySettings));
  await act(async () => mocks.dialog.change?.(true));
  mocks.state.generation++;
  await render(createElement(DataRecoverySettings));
  mocks.state.generation++;
  await render(createElement(DataRecoverySettings));
  await act(async () => pending.resolve("private-old-owner-key"));
  expect(host.textContent).not.toContain("private-old-owner-key");
});

it("retries unavailable backup status without generating a key", async () => {
  mocks.status.mockRejectedValueOnce(new Error("unavailable"));
  await render(createElement(BackupStatusSummary));
  expect(host.textContent).toContain("recovery.statusUnavailable");
  const retry = Array.from(host.querySelectorAll("button")).find(
    (button) => button.textContent === "contactVisibility.retry",
  )!;
  await act(async () => retry.click());
  expect(host.textContent).toContain("recovery.noBackup");
  expect(mocks.prepare).not.toHaveBeenCalled();
});

it("keeps the challenge and permits another attempt after cancelling the picker", async () => {
  mocks.save.mockResolvedValue(null);
  await openBackup();
  await act(async () => exportButton().click());
  expect(mocks.create).not.toHaveBeenCalled();
  expect(exportButton().disabled).toBe(false);
  expect(host.querySelector<HTMLInputElement>("input")!.value).toBe(
    "bbbb-gggg",
  );
});

it("exports with the verified per-export key and prevents a second export", async () => {
  mocks.save.mockResolvedValue("/tmp/current.mtbackup");
  mocks.create.mockResolvedValue({
    archive_bytes: 1024,
    preview: { messages: 2, created_at_ms: 1 },
  });
  await openBackup();
  await act(async () => exportButton().click());
  expect(mocks.create).toHaveBeenCalledWith(
    "/tmp/current.mtbackup",
    "aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh",
  );
  expect(host.textContent).toContain("recovery.backupComplete");
  expect(exportButton().disabled).toBe(true);
});

it("ignores stale receive policy reads and writes when runtime ownership expires", async () => {
  const pending = deferred<unknown>();
  mocks.policy.mockReturnValueOnce(pending.promise);
  await render(createElement(AttachmentReceiveSettings));
  mocks.state.runEpoch++;
  await act(async () =>
    pending.resolve({ global_auto: true, contact_auto: null }),
  );
  expect(select().value).toBe("");
  await render(createElement(AttachmentReceiveSettings));
  const write = deferred<void>();
  mocks.global.mockReturnValueOnce(write.promise);
  await act(async () => {
    select().value = "auto";
    select().dispatchEvent(new Event("change", { bubbles: true }));
  });
  mocks.state.identityEpoch++;
  await act(async () => write.resolve());
  expect(select().value).toBe("ask");
});

it("notifies the history page after a current backup succeeds", async () => {
  const onBackupCreated = vi.fn();
  mocks.save.mockResolvedValue("/tmp/current.mtbackup");
  mocks.create.mockResolvedValue({
    archive_bytes: 1024,
    preview: { messages: 2, created_at_ms: 1 },
  });
  await render(createElement(DataRecoverySettings, { onBackupCreated }));
  await act(async () => mocks.dialog.change?.(true));
  const input = host.querySelector<HTMLInputElement>("input")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, "bbbb-gggg");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => exportButton().click());
  expect(onBackupCreated).toHaveBeenCalledTimes(1);
});

it("prepares a key when the backup dialog is opened by its history page", async () => {
  const onOpenChange = vi.fn();
  await render(
    createElement(DataRecoverySettings, { open: false, onOpenChange }),
  );
  expect(mocks.prepare).not.toHaveBeenCalled();
  await render(
    createElement(DataRecoverySettings, { open: true, onOpenChange }),
  );
  expect(mocks.prepare).toHaveBeenCalledTimes(1);
  expect(host.textContent).toContain("aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh");
});

it("invalidates a pending picker when the controlling history page closes the dialog", async () => {
  const pending = deferred<string>();
  mocks.save.mockReturnValue(pending.promise);
  await render(createElement(DataRecoverySettings, { open: true }));
  const input = host.querySelector<HTMLInputElement>("input")!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, "bbbb-gggg");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => exportButton().click());
  await render(createElement(DataRecoverySettings, { open: false }));
  await act(async () => pending.resolve("/tmp/stale.mtbackup"));
  expect(mocks.create).not.toHaveBeenCalled();
  expect(host.querySelector('[data-testid="recovery-export-key"]')).toBeNull();
});

it("publishes a verified backup and releases busy state while messaging restarts", async () => {
  const pending = deferred<unknown>();
  mocks.save.mockResolvedValue("/tmp/current.mtbackup");
  mocks.create.mockReturnValue(pending.promise);
  await openBackup();
  await act(async () => exportButton().click());
  mocks.state.ready = false;
  await act(async () =>
    pending.resolve({
      archive_bytes: 1024,
      preview: { messages: 2, created_at_ms: 1 },
    }),
  );
  expect(host.textContent).toContain("recovery.backupComplete");
  const newKey = Array.from(host.querySelectorAll("button")).find(
    (button) => button.textContent === "recovery.newKey",
  )!;
  expect(newKey.disabled).toBe(false);
});
