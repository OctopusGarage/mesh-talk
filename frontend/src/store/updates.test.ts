// @vitest-environment jsdom
import { beforeEach, expect, it, vi } from "vitest";
import type { UpdateCheck } from "@/lib/types";

const api = vi.hoisted(() => ({
  check: vi.fn(),
  download: vi.fn(),
  cancel: vi.fn(),
  prepare: vi.fn(),
  install: vi.fn(),
}));
vi.mock("@/lib/api", () => ({ updates: api }));
const restart = vi.hoisted(() => ({ prepare: vi.fn(async () => {}) }));
vi.mock("./updateRestart", () => ({ prepareUpdateRestart: restart.prepare }));
const key = "mesh-talk-update-channel";
const candidate = {
  channel: "nightly",
  current_version: "0.4.3",
  latest_version: "0.4.4-nightly.42.1",
  is_newer: true,
  release_url: "https://example.com/release",
  notes: "Preview",
  package_name: "mesh-talk_v0.4.4-nightly.42.1_linux_x86_64.zip",
  package_size: 42,
  edition: "default",
} satisfies UpdateCheck;

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  localStorage.clear();
});

it("defaults to stable and restores only recognized device-local preferences", async () => {
  localStorage.setItem(key, "unknown");
  expect((await import("./updates")).useUpdates.getState().channel).toBe(
    "stable",
  );
  vi.resetModules();
  localStorage.setItem(key, "nightly");
  expect((await import("./updates")).useUpdates.getState().channel).toBe(
    "nightly",
  );
});

it("persists channel choice without checking and invalidates the previous candidate", async () => {
  const { useUpdates } = await import("./updates");
  useUpdates.setState({
    phase: "downloaded",
    release: candidate,
    download: { path: "/download", file_name: "package.zip" },
    progress: { downloaded: 42, total: 42 },
    error: "old error",
  });
  useUpdates.getState().setChannel("nightly");
  expect(localStorage.getItem(key)).toBe("nightly");
  expect(useUpdates.getState()).toMatchObject({
    channel: "nightly",
    phase: "idle",
    release: null,
    download: null,
    progress: null,
    error: null,
  });
  expect(api.check).not.toHaveBeenCalled();
});

it("forwards the selected channel and full candidate identity for a desktop download", async () => {
  const { useUpdates } = await import("./updates");
  api.check.mockResolvedValue(candidate);
  api.download.mockResolvedValue({
    path: "/download",
    file_name: candidate.package_name,
  });
  useUpdates.getState().setChannel("nightly");
  await useUpdates.getState().check();
  expect(api.check).toHaveBeenCalledWith("nightly");
  await useUpdates.getState().startDownload();
  expect(api.download).toHaveBeenCalledWith(
    candidate.latest_version,
    expect.any(Function),
    "nightly",
  );
  expect(useUpdates.getState().phase).toBe("downloaded");
});

it("prevents switching channels while a check or download is running", async () => {
  const { useUpdates } = await import("./updates");
  let finish!: (release: UpdateCheck) => void;
  api.check.mockReturnValue(
    new Promise((resolve) => {
      finish = resolve;
    }),
  );
  const checking = useUpdates.getState().check();
  useUpdates.getState().setChannel("nightly");
  expect(useUpdates.getState().channel).toBe("stable");
  finish({ ...candidate, channel: "stable" });
  await checking;
  useUpdates.setState({ phase: "downloading" });
  useUpdates.getState().setChannel("nightly");
  expect(useUpdates.getState().channel).toBe("stable");
});

it("still works when browser storage is unavailable", async () => {
  const spy = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
    throw new Error("blocked");
  });
  const { useUpdates } = await import("./updates");
  expect(useUpdates.getState().channel).toBe("stable");
  spy.mockRestore();
  const write = vi
    .spyOn(Storage.prototype, "setItem")
    .mockImplementation(() => {
      throw new Error("blocked");
    });
  useUpdates.getState().setChannel("nightly");
  expect(useUpdates.getState().channel).toBe("nightly");
  write.mockRestore();
});

it("downloads a signed update, saves drafts and delegates install/relaunch once", async () => {
  const { useUpdates } = await import("./updates");
  api.check.mockResolvedValue({ ...candidate, automatic_install: true });
  api.prepare.mockResolvedValue({
    path: "/private/update",
    file_name: "app.tar.gz",
    installable: true,
  });
  api.install.mockImplementation(async (_version, progress) => {
    progress("restarting");
  });
  await useUpdates.getState().check();
  await useUpdates.getState().startDownload();
  expect(api.prepare).toHaveBeenCalledWith(
    candidate.latest_version,
    expect.any(Function),
    "stable",
  );
  expect(restart.prepare).toHaveBeenCalledOnce();
  expect(api.install).toHaveBeenCalledOnce();
  expect(useUpdates.getState().phase).toBe("restarting");
});

it("keeps a verified update ready when active work defers restart, and retries without downloading again", async () => {
  const { useUpdates } = await import("./updates");
  api.check.mockResolvedValue({ ...candidate, automatic_install: true });
  api.prepare.mockResolvedValue({
    path: "/private/update",
    file_name: "app.tar.gz",
    installable: true,
  });
  restart.prepare.mockRejectedValueOnce(
    new Error("Finish the current call before updating."),
  );
  await useUpdates.getState().check();
  await useUpdates.getState().startDownload();
  expect(useUpdates.getState()).toMatchObject({
    phase: "downloaded",
    error: "Finish the current call before updating.",
  });
  expect(api.install).not.toHaveBeenCalled();
  await useUpdates.getState().install();
  expect(api.prepare).toHaveBeenCalledOnce();
  expect(api.install).toHaveBeenCalledOnce();
});

it("preserves staged state after installation failure and prevents competing operations", async () => {
  const { useUpdates } = await import("./updates");
  useUpdates.setState({
    phase: "downloaded",
    release: candidate,
    download: {
      path: "/private/update",
      file_name: "app.tar.gz",
      installable: true,
    },
  });
  api.install.mockImplementation(async () => {
    useUpdates.getState().setChannel("nightly");
    await useUpdates.getState().check();
    await useUpdates.getState().install();
    throw new Error("Authorization canceled");
  });
  await useUpdates.getState().install();
  expect(useUpdates.getState()).toMatchObject({
    phase: "downloaded",
    channel: "stable",
    error: "Authorization canceled",
  });
  expect(api.check).not.toHaveBeenCalled();
  expect(api.install).toHaveBeenCalledOnce();
});

it("never installs when cancellation races with a completed native preparation", async () => {
  const { useUpdates } = await import("./updates");
  api.check.mockResolvedValue({ ...candidate, automatic_install: true });
  let finish!: (value: {
    path: string;
    file_name: string;
    installable: boolean;
  }) => void;
  api.prepare.mockReturnValue(
    new Promise((resolve) => {
      finish = resolve;
    }),
  );
  await useUpdates.getState().check();
  const preparing = useUpdates.getState().startDownload();
  await useUpdates.getState().cancel();
  finish({
    path: "/private/payload",
    file_name: "app.tar.gz",
    installable: true,
  });
  await preparing;
  expect(api.install).not.toHaveBeenCalled();
  expect(useUpdates.getState()).toMatchObject({
    phase: "ready",
    download: null,
    error: null,
  });
});

it("honors cancellation intent even when notifying the native downloader fails", async () => {
  const { useUpdates } = await import("./updates");
  api.check.mockResolvedValue({ ...candidate, automatic_install: true });
  let finish!: (value: {
    path: string;
    file_name: string;
    installable: boolean;
  }) => void;
  api.prepare.mockReturnValue(
    new Promise((resolve) => {
      finish = resolve;
    }),
  );
  api.cancel.mockRejectedValueOnce(
    new Error("Native cancellation unavailable"),
  );
  await useUpdates.getState().check();
  const preparing = useUpdates.getState().startDownload();
  await useUpdates.getState().cancel();
  finish({
    path: "/private/payload",
    file_name: "app.tar.gz",
    installable: true,
  });
  await preparing;
  expect(api.install).not.toHaveBeenCalled();
});
