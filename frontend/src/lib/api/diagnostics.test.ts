import { beforeEach, expect, it, vi } from "vitest";
import type { DownloadProgress } from "@/lib/types";
const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
  Channel: class {
    onmessage?: (value: unknown) => void;
  },
}));
import { updates } from "./diagnostics";

beforeEach(() => vi.resetAllMocks());

it("defaults legacy checks to Stable and forwards explicit Nightly choice to native authority", async () => {
  const release = { current_version: "0.4.4-dev", channel: "stable" };
  mocks.invoke.mockResolvedValueOnce(release);
  expect(await updates.check()).toBe(release);
  expect(mocks.invoke).toHaveBeenCalledWith("check_for_update", {
    updateChannel: "stable",
  });
  await updates.check("nightly");
  expect(mocks.invoke).toHaveBeenLastCalledWith("check_for_update", {
    updateChannel: "nightly",
  });
});

it("forwards the exact checked Nightly identity, channel and native progress messages", async () => {
  const onProgress = vi.fn<(value: DownloadProgress) => void>();
  const downloaded = {
    path: "/Downloads/nightly.zip",
    file_name: "nightly.zip",
  };
  mocks.invoke.mockResolvedValue(downloaded);
  const result = await updates.download(
    "0.4.4-nightly.42.2",
    onProgress,
    "nightly",
  );
  expect(result).toBe(downloaded);
  expect(mocks.invoke).toHaveBeenCalledWith("download_update", {
    version: "0.4.4-nightly.42.2",
    updateChannel: "nightly",
    onProgress: expect.objectContaining({ onmessage: expect.any(Function) }),
  });
  const progressChannel = mocks.invoke.mock.calls[0][1].onProgress;
  progressChannel.onmessage({ downloaded: 21, total: 42 });
  expect(onProgress).toHaveBeenCalledWith({ downloaded: 21, total: 42 });
});

it("keeps legacy downloads Stable and delegates cancellation to the native download owner", async () => {
  await updates.download("0.4.3", vi.fn());
  expect(mocks.invoke).toHaveBeenCalledWith(
    "download_update",
    expect.objectContaining({ version: "0.4.3", updateChannel: "stable" }),
  );
  await updates.cancel();
  expect(mocks.invoke).toHaveBeenLastCalledWith("cancel_update_download");
});

it("prepares only the selected build and installs host-owned bytes without accepting paths", async () => {
  await updates.prepare("0.4.4-nightly.42.1", vi.fn(), "nightly");
  expect(mocks.invoke).toHaveBeenLastCalledWith(
    "prepare_update",
    expect.objectContaining({
      version: "0.4.4-nightly.42.1",
      updateChannel: "nightly",
      onProgress: expect.any(Object),
    }),
  );
  const progress = vi.fn();
  await updates.install("0.4.4-nightly.42.1", progress, "nightly");
  expect(mocks.invoke).toHaveBeenLastCalledWith("install_update", {
    version: "0.4.4-nightly.42.1",
    updateChannel: "nightly",
    onProgress: expect.any(Object),
  });
  mocks.invoke.mock.calls[
    mocks.invoke.mock.calls.length - 1
  ][1].onProgress.onmessage("restarting");
  expect(progress).toHaveBeenCalledWith("restarting");
});
