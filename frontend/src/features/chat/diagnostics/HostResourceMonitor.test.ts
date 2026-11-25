// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { expect, it, vi } from "vitest";
import { obs } from "@/lib/api";
import { HostResourceMonitor } from "./HostResourceMonitor";

const nativeVisibility = vi.hoisted(() => ({
  minimized: false,
  observers: new Set<() => void>(),
}));
vi.mock("@/lib/nativeWindowVisibility", () => ({
  isNativeWindowMinimized: () => nativeVisibility.minimized,
  subscribeNativeWindowMinimized: (observer: () => void) => {
    nativeVisibility.observers.add(observer);
    return () => {
      nativeVisibility.observers.delete(observer);
    };
  },
}));

vi.mock("@/lib/api", () => ({
  obs: {
    hostResources: vi.fn(),
    startHostResourceSampling: vi.fn(),
    stopHostResourceSampling: vi.fn(),
  },
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: { size?: string; pid?: number }) =>
      options?.size
        ? options.size + "/s"
        : options?.pid
          ? String(options.pid)
          : key,
    i18n: { language: "en" },
  }),
}));

it("samples only when enabled and visible, and discards readings when stopped", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
  const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const read = vi.mocked(obs.hostResources);
  const start = vi.mocked(obs.startHostResourceSampling);
  const stop = vi.mocked(obs.stopHostResourceSampling);
  start.mockResolvedValueOnce(1).mockResolvedValueOnce(2);
  stop.mockResolvedValue(undefined);
  const sample = (cpuMs: number, birth = "child") => ({
    processes: [
      {
        pid: 10,
        birth: "host",
        cpu_ms: cpuMs,
        resident_bytes: 15_000_000,
        disk_write_bytes: 0,
        io_write_bytes: null,
      },
      {
        pid: 11,
        birth,
        cpu_ms: cpuMs,
        resident_bytes: 5_000_000,
        disk_write_bytes: 0,
        io_write_bytes: null,
      },
    ],
    sampled_at_ms: 1_000,
    bounded: false,
    authenticated_channels: 2,
    sent_frame_bytes: 1_500,
    received_frame_bytes: 900,
  });
  read.mockResolvedValue(sample(100));
  const container = document.createElement("div");
  const root = createRoot(container);
  const advance = async () => {
    await act(async () => vi.advanceTimersByTimeAsync(3000));
  };
  try {
    await act(async () => root.render(createElement(HostResourceMonitor)));
    await advance();
    expect(read).not.toHaveBeenCalled();

    await act(async () => container.querySelector("button")!.click());
    expect(read).toHaveBeenCalledTimes(1);
    expect(container.textContent).toContain("20 MB");
    expect(read).toHaveBeenLastCalledWith(1, false);
    expect(container.textContent).toContain("1.5 kB");
    read.mockResolvedValue({
      ...sample(115),
      sampled_at_ms: 3_000,
      sent_frame_bytes: 7_500,
      processes: sample(115).processes.map((entry) => ({
        ...entry,
        resident_bytes: 20_000_000,
        disk_write_bytes: 4_500,
      })),
    });
    await advance();
    expect(read).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain("1.5%");
    expect(container.textContent).toContain("4.5 kB/s");
    expect(container.textContent).toContain("3 kB/s");
    expect(container.textContent).not.toContain("2 kB/s");
    expect(container.textContent).toContain("40 MB");
    const processDetails = container.querySelector("details")!;
    expect(processDetails.querySelector("li")).toBeNull();
    await act(async () => {
      processDetails.open = true;
      processDetails.dispatchEvent(new Event("toggle"));
    });
    expect(processDetails.querySelector("li")?.textContent).toContain("10");
    await act(async () => {
      processDetails.open = false;
      processDetails.dispatchEvent(new Event("toggle"));
    });
    expect(processDetails.querySelector("li")).toBeNull();

    read.mockResolvedValue(sample(130, "replacement"));
    await advance();
    expect(read).toHaveBeenCalledTimes(3);
    expect(container.textContent).not.toContain("1.5%");
    expect(container.textContent).not.toContain("4.5 kB/s");
    expect(container.textContent).not.toContain("3 kB/s");
    expect(container.textContent).toContain("40 MB");

    hidden.mockReturnValue(true);
    await act(async () =>
      document.dispatchEvent(new Event("visibilitychange")),
    );
    await advance();
    expect(read).toHaveBeenCalledTimes(3);
    expect(stop).toHaveBeenCalledWith(1);

    hidden.mockReturnValue(false);
    await act(async () =>
      document.dispatchEvent(new Event("visibilitychange")),
    );
    expect(read).toHaveBeenLastCalledWith(2, false);
    expect(container.textContent).not.toContain("1.5%");
    await act(async () => container.querySelector("button")!.click());
    expect(stop).toHaveBeenCalledWith(2);
    await advance();
    expect(read).toHaveBeenCalledTimes(4);
    expect(container.querySelector("dl")).toBeNull();
  } finally {
    await act(async () => root.unmount());
    vi.unstubAllGlobals();
    vi.useRealTimers();
    hidden.mockRestore();
  }
});

it("loads message details on demand and stops permanently when its sampling lease expires", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  vi.mocked(obs.startHostResourceSampling).mockReset().mockResolvedValue(91);
  vi.mocked(obs.stopHostResourceSampling)
    .mockReset()
    .mockResolvedValue(undefined);
  const messages = {
    accepted: 5,
    delivered: 3,
    rejected: 1,
    send_request_errors: 2,
    route_restarts: 1,
    direct_storage_holds: 0,
    relay_storage_holds: 0,
    attachments_accepted: 2,
    attachments_declined: 1,
    attempt_timeouts: 1,
    retries_scheduled: 2,
    received_direct: [{ count: 3, bytes: 900 }],
    received_group: [],
    catch_up: 2,
    history_sync: 1,
    own_device_sync: 1,
    duplicate_direct_events: 0,
    attachments_verified: 1,
    verified_attachment_bytes: 2000,
    delivery_latency: [1, 0, 0, 0, 0, 0, 0, 1],
  };
  const sample = {
    sampled_at_ms: 1000,
    processes: [
      {
        pid: 4,
        birth: "host",
        cpu_ms: 20,
        resident_bytes: null,
        disk_write_bytes: null,
        io_write_bytes: 500,
      },
    ],
    bounded: true,
    authenticated_channels: 1,
    sent_frame_bytes: 20,
    received_frame_bytes: 10,
    messages,
    direct_backlog: [2, 450] as [number, number],
  };
  const read = vi
    .mocked(obs.hostResources)
    .mockReset()
    .mockResolvedValue(sample);
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(HostResourceMonitor)));
    await act(async () => container.querySelector("button")!.click());
    expect(container.textContent).toContain("diagnostics.resources.bounded");
    expect(container.textContent).toContain("500 byte");
    const details = container.querySelectorAll("details")[1];
    await act(async () => {
      details.open = true;
      details.dispatchEvent(new Event("toggle"));
    });
    expect(container.querySelectorAll("table tbody tr")).toHaveLength(4);
    expect(container.textContent).toContain("≤");
    expect(container.textContent).toContain("> ");
    read.mockResolvedValue({
      ...sample,
      sampled_at_ms: 4000,
      bounded: false,
      direct_backlog: null,
      messages: {
        ...messages,
        delivery_latency: [],
        verified_attachment_bytes: 3000,
      },
      processes: [{ ...sample.processes[0], cpu_ms: 10, io_write_bytes: 800 }],
    });
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(read).toHaveBeenLastCalledWith(91, true);
    expect(container.textContent).toContain("333.3 byte/s");
    read.mockResolvedValue(null);
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(container.querySelector("[role=status]")?.textContent).toBe(
      "diagnostics.resources.unavailable",
    );
    expect(obs.stopHostResourceSampling).toHaveBeenCalledWith(91);
    const calls = read.mock.calls.length;
    await act(async () => vi.advanceTimersByTimeAsync(9000));
    expect(read).toHaveBeenCalledTimes(calls);
  } finally {
    await act(async () => root.unmount());
    hidden.mockRestore();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  }
});

it("releases a lease issued after monitoring was stopped", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  let finish!: (id: number) => void;
  vi.mocked(obs.startHostResourceSampling)
    .mockReset()
    .mockReturnValue(
      new Promise<number>((resolve) => {
        finish = resolve;
      }),
    );
  vi.mocked(obs.stopHostResourceSampling)
    .mockReset()
    .mockResolvedValue(undefined);
  vi.mocked(obs.hostResources).mockReset();
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(HostResourceMonitor)));
    await act(async () => container.querySelector("button")!.click());
    await act(async () => container.querySelector("button")!.click());
    await act(async () => finish(92));
    expect(obs.stopHostResourceSampling).toHaveBeenCalledWith(92);
    expect(obs.hostResources).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
    vi.unstubAllGlobals();
  }
});

it("computes stable CPU intervals and invalidates an interval after a long sampling gap", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  vi.mocked(obs.startHostResourceSampling).mockReset().mockResolvedValue(93);
  vi.mocked(obs.stopHostResourceSampling)
    .mockReset()
    .mockResolvedValue(undefined);
  const sample = {
    sampled_at_ms: 1000,
    processes: [
      {
        pid: 4,
        birth: "host",
        cpu_ms: 100,
        resident_bytes: 100,
        disk_write_bytes: 20,
        io_write_bytes: 20,
      },
    ],
    bounded: false,
    authenticated_channels: 0,
    sent_frame_bytes: 50,
    received_frame_bytes: 40,
  };
  const read = vi
    .mocked(obs.hostResources)
    .mockReset()
    .mockResolvedValue(sample);
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(HostResourceMonitor)));
    await act(async () => container.querySelector("button")!.click());
    read.mockResolvedValue({
      ...sample,
      sampled_at_ms: 4000,
      processes: [{ ...sample.processes[0], cpu_ms: 130, io_write_bytes: 320 }],
    });
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(container.textContent).toContain("1%");
    expect(container.textContent).toContain("100 byte/s");
    read.mockResolvedValue({
      ...sample,
      sampled_at_ms: 7000,
      processes: [{ ...sample.processes[0], birth: "replacement" }],
    });
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(container.textContent).not.toContain("1%");
    read.mockResolvedValue({ ...sample, sampled_at_ms: 15000 });
    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(container.textContent).not.toContain("1%");
    await act(async () => {
      const details = container.querySelector("details")!;
      details.open = true;
      details.dispatchEvent(new Event("toggle"));
    });
    expect(container.querySelector("li")?.textContent).toContain("—");
  } finally {
    await act(async () => root.unmount());
    hidden.mockRestore();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  }
});

it("releases a pending sampling lease when the native window is minimized", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  let finish!: (id: number) => void;
  vi.mocked(obs.startHostResourceSampling)
    .mockReset()
    .mockReturnValue(
      new Promise<number>((resolve) => {
        finish = resolve;
      }),
    );
  vi.mocked(obs.stopHostResourceSampling)
    .mockReset()
    .mockResolvedValue(undefined);
  vi.mocked(obs.hostResources).mockReset();
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(HostResourceMonitor)));
    await act(async () => container.querySelector("button")!.click());
    nativeVisibility.minimized = true;
    await act(async () => {
      for (const observer of nativeVisibility.observers) observer();
    });
    await act(async () => finish(94));
    expect(obs.stopHostResourceSampling).toHaveBeenCalledWith(94);
    expect(obs.hostResources).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
    nativeVisibility.minimized = false;
    hidden.mockRestore();
    vi.unstubAllGlobals();
  }
});

it("cancels sampling before admission without stopping a nonexistent lease", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  vi.mocked(obs.startHostResourceSampling).mockReset();
  vi.mocked(obs.stopHostResourceSampling).mockReset();
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(HostResourceMonitor)));
    await act(async () => {
      flushSync(() => container.querySelector("button")!.click());
      nativeVisibility.minimized = true;
      for (const observer of nativeVisibility.observers) observer();
    });
    expect(obs.startHostResourceSampling).not.toHaveBeenCalled();
    expect(obs.stopHostResourceSampling).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
    nativeVisibility.minimized = false;
    hidden.mockRestore();
    vi.unstubAllGlobals();
  }
});
it("ignores a sampling error after minimization has already released its lease", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  let fail!: (error: Error) => void;
  vi.mocked(obs.startHostResourceSampling).mockReset().mockResolvedValue(95);
  vi.mocked(obs.stopHostResourceSampling)
    .mockReset()
    .mockResolvedValue(undefined);
  vi.mocked(obs.hostResources)
    .mockReset()
    .mockReturnValue(
      new Promise<null>((_, reject) => {
        fail = reject;
      }),
    );
  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () => root.render(createElement(HostResourceMonitor)));
    await act(async () => container.querySelector("button")!.click());
    nativeVisibility.minimized = true;
    await act(async () => {
      for (const observer of nativeVisibility.observers) observer();
    });
    await act(async () => fail(new Error("retired sampling")));
    expect(obs.stopHostResourceSampling).toHaveBeenCalledTimes(1);
    expect(container.textContent).not.toContain(
      "diagnostics.resources.unavailable",
    );
  } finally {
    await act(async () => root.unmount());
    nativeVisibility.minimized = false;
    hidden.mockRestore();
    vi.unstubAllGlobals();
  }
});
