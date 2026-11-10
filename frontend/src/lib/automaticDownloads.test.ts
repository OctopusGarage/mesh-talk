import { beforeEach, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({
  save: vi.fn(),
  dir: vi.fn(),
  remember: vi.fn(),
  saved: vi.fn(),
  current: true,
}));
vi.mock("@/lib/api", () => ({ chat: { saveFileToDir: mocks.save } }));
vi.mock("@/lib/download", () => ({ effectiveDownloadDir: mocks.dir }));
vi.mock("@/lib/savedDownloads", () => ({
  rememberSavedDownloadForOwner: mocks.remember,
  savedDownloadPath: mocks.saved,
}));
vi.mock("@/store/chat", async () => {
  const { create } = await import("zustand");
  return {
    captureChatOwnership: () => ({
      owner: "alice",
      current: () => mocks.current,
    }),
    useChat: create(() => ({ incomingFiles: [], ready: true })),
  };
});
vi.mock("@/store/fileAvailability", async () => {
  const { create } = await import("zustand");
  return {
    useFileAvailability: create((set) => ({
      statuses: {},
      merge: (statuses: { file_conv: string }[]) =>
        set((state: { statuses: Record<string, unknown> }) => ({
          statuses: {
            ...state.statuses,
            ...Object.fromEntries(
              statuses.map((status) => [status.file_conv, status]),
            ),
          },
        })),
    })),
    watchFileAvailability: () => () => {},
    refreshFileAvailability: async () => {},
    markUnavailableFileError: (fileConv: string, error: unknown) => {
      if (!String(error).includes("unknown file")) return false;
      useFileAvailability.setState({
        statuses: { [fileConv]: { ready: false, unavailable: true } },
      } as never);
      return true;
    },
  };
});
import {
  startAutomaticDownloads,
  useAutomaticDownloads,
  retryAutomaticDownload,
} from "./automaticDownloads";
import { useChat } from "@/store/chat";
import { useFileAvailability } from "@/store/fileAvailability";
const file = {
  fileConv: "file",
  name: "report.pdf",
  mime: "application/pdf",
  media: false,
  size: 10,
  fromName: "Alice",
};
const tick = async () => {
  for (let i = 0; i < 10; i++) await Promise.resolve();
};
beforeEach(() => {
  vi.clearAllMocks();
  mocks.current = true;
  mocks.dir.mockResolvedValue("/Downloads");
  mocks.save.mockResolvedValue("/Downloads/report (1).pdf");
  mocks.saved.mockReturnValue(undefined);
  useChat.setState({ incomingFiles: [file], ready: true });
  useFileAvailability.setState({
    statuses: { file: { file_conv: "file", ready: true, consent: "accepted" } },
  } as never);
});
it("saves verified accepted files without a mounted card and remembers the actual path", async () => {
  const stop = startAutomaticDownloads();
  await tick();
  expect(mocks.save).toHaveBeenCalledWith("file", "/Downloads", "alice");
  expect(mocks.remember).toHaveBeenCalledWith(
    "alice",
    "file",
    "/Downloads/report (1).pdf",
  );
  stop();
});
it.each(["pending", "declined"])(
  "does not export %s consent",
  async (consent) => {
    useFileAvailability.setState({
      statuses: { file: { ready: true, consent } },
    } as never);
    const stop = startAutomaticDownloads();
    await tick();
    expect(mocks.save).not.toHaveBeenCalled();
    stop();
  },
);
it("waits for verified bytes and excludes inline images", async () => {
  useChat.setState({
    incomingFiles: [{ ...file, media: true, mime: "image/png" }],
  });
  const stop = startAutomaticDownloads();
  await tick();
  expect(mocks.save).not.toHaveBeenCalled();
  stop();
});
it("reports failed writes once and retries only explicitly", async () => {
  mocks.save.mockRejectedValueOnce(new Error("disk full"));
  const stop = startAutomaticDownloads();
  await tick();
  expect(useAutomaticDownloads.getState().failures.file).toBe("disk full");
  useFileAvailability.setState({
    statuses: { ...useFileAvailability.getState().statuses },
  });
  await tick();
  expect(mocks.save).toHaveBeenCalledTimes(1);
  retryAutomaticDownload("file");
  await tick();
  expect(mocks.save).toHaveBeenCalledTimes(2);
  stop();
});
it("does not write after ownership changes while resolving the folder", async () => {
  mocks.dir.mockImplementationOnce(async () => {
    mocks.current = false;
    return "/Downloads";
  });
  const stop = startAutomaticDownloads();
  await tick();
  expect(mocks.save).not.toHaveBeenCalled();
  expect(mocks.remember).not.toHaveBeenCalled();
  stop();
});
it("records a completed export for its captured owner after stop", async () => {
  let complete!: (path: string) => void;
  mocks.save.mockImplementationOnce(
    () =>
      new Promise<string>((resolve) => {
        complete = resolve;
      }),
  );
  const stop = startAutomaticDownloads();
  await tick();
  stop();
  complete("/Downloads/report.pdf");
  await tick();
  expect(mocks.remember).toHaveBeenCalledWith(
    "alice",
    "file",
    "/Downloads/report.pdf",
  );
});
it("waits for verification before saving an accepted transfer", async () => {
  useFileAvailability.setState({
    statuses: { file: { ready: false, consent: "accepted" } },
  } as never);
  const stop = startAutomaticDownloads();
  await tick();
  expect(mocks.save).not.toHaveBeenCalled();
  useFileAvailability.setState({
    statuses: { file: { ready: true, consent: "accepted" } },
  } as never);
  await tick();
  expect(mocks.save).toHaveBeenCalledTimes(1);
  stop();
});
it("does not export an already remembered destination", async () => {
  mocks.saved.mockReturnValue("/Downloads/report.pdf");
  const stop = startAutomaticDownloads();
  await tick();
  expect(mocks.save).not.toHaveBeenCalled();
  stop();
});
it("processes queued files serially", async () => {
  useChat.setState({ incomingFiles: [file, { ...file, fileConv: "second" }] });
  useFileAvailability.setState({
    statuses: { file: { ready: true }, second: { ready: true } },
  } as never);
  let complete!: (path: string) => void;
  mocks.save.mockImplementationOnce(
    () =>
      new Promise<string>((resolve) => {
        complete = resolve;
      }),
  );
  const stop = startAutomaticDownloads();
  await tick();
  expect(mocks.save).toHaveBeenCalledTimes(1);
  complete("/Downloads/report.pdf");
  await tick();
  expect(mocks.save).toHaveBeenCalledTimes(2);
  stop();
});
it("restores only this owner's pending downloads after restarting", async () => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  });
  useFileAvailability.setState({ statuses: {} });
  let stop = startAutomaticDownloads();
  await tick();
  stop();
  useChat.setState({ incomingFiles: [] });
  stop = startAutomaticDownloads();
  await tick();
  expect(useChat.getState().incomingFiles).toEqual([file]);
  stop();
  vi.unstubAllGlobals();
});

it("ignores another owner's persisted queue", async () => {
  vi.stubGlobal("localStorage", {
    getItem: (key: string) =>
      key.endsWith(":bob") ? JSON.stringify([file]) : null,
    setItem: vi.fn(),
  });
  useChat.setState({ incomingFiles: [] });
  const stop = startAutomaticDownloads();
  await tick();
  expect(useChat.getState().incomingFiles).toEqual([]);
  expect(mocks.save).not.toHaveBeenCalled();
  stop();
  vi.unstubAllGlobals();
});
it("marks content removed during export unavailable without a write-failure retry", async () => {
  mocks.save.mockRejectedValueOnce(new Error("unknown file"));
  const stop = startAutomaticDownloads();
  await tick();
  expect(useAutomaticDownloads.getState().failures.file).toBeUndefined();
  expect(useFileAvailability.getState().statuses.file.unavailable).toBe(true);
  stop();
});
it("does not start another native export while a stopped same-owner worker is saving", async () => {
  let complete!: (path: string) => void;
  mocks.save.mockImplementationOnce(
    () =>
      new Promise<string>((resolve) => {
        complete = resolve;
      }),
  );
  let stop = startAutomaticDownloads();
  await tick();
  stop();
  stop = startAutomaticDownloads();
  await tick();
  expect(mocks.save).toHaveBeenCalledTimes(1);
  complete("/Downloads/report.pdf");
  await tick();
  expect(mocks.save).toHaveBeenCalledTimes(1);
  expect(useAutomaticDownloads.getState().saving).toBeNull();
  stop();
});
it("retains pending transfers evicted from the tray and resumes them after restart", async () => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  });
  useFileAvailability.setState({ statuses: {} });
  let stop = startAutomaticDownloads();
  await tick();
  useChat.setState({ incomingFiles: [{ ...file, fileConv: "second" }] });
  await tick();
  stop();
  useChat.setState({ incomingFiles: [] });
  stop = startAutomaticDownloads();
  await tick();
  expect(
    useChat.getState().incomingFiles.map((entry) => entry.fileConv),
  ).toEqual(expect.arrayContaining(["file", "second"]));
  useFileAvailability.setState({
    statuses: { file: { ready: true }, second: { ready: true } },
  } as never);
  await tick();
  expect(mocks.save).toHaveBeenCalledTimes(2);
  stop();
  vi.unstubAllGlobals();
});
it("wakes a replacement worker for the next export after the retired export completes", async () => {
  let completeFirst!: (path: string) => void;
  let completeSecond!: (path: string) => void;
  mocks.save
    .mockImplementationOnce(
      () =>
        new Promise<string>((resolve) => {
          completeFirst = resolve;
        }),
    )
    .mockImplementationOnce(
      () =>
        new Promise<string>((resolve) => {
          completeSecond = resolve;
        }),
    );
  let stop = startAutomaticDownloads();
  await tick();
  stop();
  useChat.setState({ incomingFiles: [file, { ...file, fileConv: "second" }] });
  useFileAvailability.setState({
    statuses: { file: { ready: true }, second: { ready: true } },
  } as never);
  stop = startAutomaticDownloads();
  await tick();
  expect(mocks.save).toHaveBeenCalledTimes(1);
  completeFirst("/Downloads/report.pdf");
  await tick();
  expect(mocks.save).toHaveBeenCalledTimes(2);
  expect(mocks.save).toHaveBeenLastCalledWith("second", "/Downloads", "alice");
  expect(useAutomaticDownloads.getState().saving).toBe("second");
  completeSecond("/Downloads/report (1).pdf");
  await tick();
  stop();
});
it("reports storage quota failures while continuing the current-session export", async () => {
  vi.stubGlobal("localStorage", {
    getItem: () => null,
    setItem: () => {
      throw new Error("quota exceeded");
    },
  });
  const stop = startAutomaticDownloads();
  await tick();
  expect(mocks.save).toHaveBeenCalledTimes(1);
  expect(useAutomaticDownloads.getState().persistenceError).toBe(
    "quota exceeded",
  );
  stop();
  vi.unstubAllGlobals();
});
it("explicit dismissal removes pending work while tray eviction does not", async () => {
  const { notifyIncomingFileDismissed } =
    await import("./incomingFileDismissal");
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  });
  useFileAvailability.setState({ statuses: {} });
  const stop = startAutomaticDownloads();
  await tick();
  useChat.setState({ incomingFiles: [] });
  notifyIncomingFileDismissed("file");
  await tick();
  expect(JSON.parse(values.get("mesh-talk-pending-downloads:alice")!)).toEqual(
    [],
  );
  stop();
  vi.unstubAllGlobals();
});
it("preserves tray-evicted work when restarting during a paused native export", async () => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  });
  let complete!: (path: string) => void;
  mocks.save.mockImplementationOnce(
    () =>
      new Promise<string>((resolve) => {
        complete = resolve;
      }),
  );
  let stop = startAutomaticDownloads();
  await tick();
  useChat.setState({ incomingFiles: [{ ...file, fileConv: "second" }] });
  useFileAvailability.setState({
    statuses: { file: { ready: true }, second: { ready: true } },
  } as never);
  await tick();
  stop();
  useChat.setState({ incomingFiles: [] });
  stop = startAutomaticDownloads();
  await tick();
  expect(
    JSON.parse(values.get("mesh-talk-pending-downloads:alice")!).map(
      (entry: { fileConv: string }) => entry.fileConv,
    ),
  ).toEqual(expect.arrayContaining(["file", "second"]));
  expect(mocks.save).toHaveBeenCalledTimes(1);
  complete("/Downloads/report.pdf");
  await tick();
  expect(mocks.save).toHaveBeenCalledTimes(2);
  expect(mocks.remember).toHaveBeenCalledWith(
    "alice",
    "file",
    "/Downloads/report.pdf",
  );
  expect(mocks.remember).toHaveBeenCalledWith(
    "alice",
    "second",
    "/Downloads/report (1).pdf",
  );
  expect(JSON.parse(values.get("mesh-talk-pending-downloads:alice")!)).toEqual(
    [],
  );
  stop();
  vi.unstubAllGlobals();
});
it("never automatically exports protected images even when mislabeled as generic files", async () => {
  useChat.setState({
    incomingFiles: [
      { ...file, protection: "click", mime: "image/png", media: false },
    ],
  });
  useFileAvailability.setState({
    statuses: { file: { ready: true, total: 1, done: 1 } },
  } as never);
  const stop = startAutomaticDownloads();
  await Promise.resolve();
  await Promise.resolve();
  expect(mocks.save).not.toHaveBeenCalled();
  stop();
});
