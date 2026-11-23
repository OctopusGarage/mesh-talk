// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { obs } from "@/lib/api";
import { HostResourceMonitor } from "./HostResourceMonitor";

vi.mock("@/lib/api", () => ({
  obs: { hostResources: vi.fn() },
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key, i18n: { language: "en" } }),
}));

it("samples only when enabled and visible, and discards readings when stopped", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
  const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const read = vi.mocked(obs.hostResources);
  read.mockResolvedValue({
    cpu_ms: 100,
    resident_bytes: 20_000_000,
    disk_write_bytes: null,
  });
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
    read.mockResolvedValue({
      cpu_ms: 130,
      resident_bytes: 20_000_000,
      disk_write_bytes: 3_000_000,
    });
    await advance();
    expect(read).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain("1%");

    hidden.mockReturnValue(true);
    await act(async () =>
      document.dispatchEvent(new Event("visibilitychange")),
    );
    await advance();
    expect(read).toHaveBeenCalledTimes(2);

    await act(async () => container.querySelector("button")!.click());
    hidden.mockReturnValue(false);
    await act(async () =>
      document.dispatchEvent(new Event("visibilitychange")),
    );
    await advance();
    expect(read).toHaveBeenCalledTimes(2);
    expect(container.querySelector("dl")).toBeNull();
  } finally {
    await act(async () => root.unmount());
    vi.unstubAllGlobals();
    vi.useRealTimers();
    hidden.mockRestore();
  }
});
