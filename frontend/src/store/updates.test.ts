// @vitest-environment jsdom
import { beforeEach, expect, it, vi } from "vitest";
import type { UpdateCheck } from "@/lib/types";

const api = vi.hoisted(() => ({
  check: vi.fn(),
  download: vi.fn(),
  cancel: vi.fn(),
}));
vi.mock("@/lib/api", () => ({ updates: api }));
const key = "mesh-talk-update-channel";
const candidate = {
  channel: "nightly",
  current_version: "0.4.3",
  latest_version: "0.4.3-nightly.42.1",
  is_newer: true,
  release_url: "https://example.com/release",
  notes: "Preview",
  package_name: "mesh-talk_v0.4.3-nightly.42.1_linux_x86_64_lite.zip",
  package_size: 42,
  edition: "lite",
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

it("forwards the selected channel and full candidate identity for a lite download", async () => {
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
