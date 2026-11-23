// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { obs } from "@/lib/api";
import { HostResourceMonitor } from "./HostResourceMonitor";

vi.mock("@/lib/api", () => ({
  obs: {
    hostResources: vi.fn(),
    startHostResourceSampling: vi.fn(),
    stopHostResourceSampling: vi.fn(),
  },
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key, i18n: { language: "en" } }),
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
        disk_write_bytes: null,
        io_write_bytes: null,
      },
      {
        pid: 11,
        birth,
        cpu_ms: cpuMs,
        resident_bytes: 5_000_000,
        disk_write_bytes: null,
        io_write_bytes: null,
      },
    ],
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
    expect(read).toHaveBeenLastCalledWith(1);
    expect(container.textContent).toContain("1.5 kB");
    read.mockResolvedValue(sample(115));
    await advance();
    expect(read).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain("1%");

    read.mockResolvedValue(sample(130, "replacement"));
    await advance();
    expect(read).toHaveBeenCalledTimes(3);
    expect(container.textContent).not.toContain("1%");

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
    expect(read).toHaveBeenLastCalledWith(2);
    expect(container.textContent).not.toContain("1%");
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
