// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { beforeEach, expect, it, vi } from "vitest";
import { useAuth } from "./auth";
const api = vi.hoisted(() => ({ get: vi.fn() }));
vi.mock("@/lib/api/accountProfile", () => ({ accountProfile: api }));
import { useAccountProfiles, useAccountProfile } from "./accountProfiles";
const profile = (id: string, bio: string) => ({
  account_id: id,
  display_name: id,
  bio,
  created_at_ms: null,
});
beforeEach(() => {
  useAuth.setState({
    user: { id: "owner", username: "owner", display_name: "Owner" },
    generation: 1,
  });
  useAccountProfiles.getState().clear();
  api.get.mockReset();
});
it("releases a timed-out profile lane so a new observation can recover", async () => {
  vi.useFakeTimers();
  let finish!: (value: ReturnType<typeof profile>) => void;
  api.get.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = useAccountProfiles.getState().load("bob");
  const rejected = expect(pending).rejects.toThrow("timed out");
  try {
    await vi.advanceTimersByTimeAsync(10_000);
    expect(api.get).toHaveBeenCalledTimes(1);
    // Attach rejection before advancing the timer to keep unhandled errors visible.
    await rejected;
    api.get.mockResolvedValueOnce(profile("bob", "fresh"));
    await useAccountProfiles.getState().load("bob");
    finish(profile("bob", "stale"));
    await vi.advanceTimersByTimeAsync(0);
    expect(useAccountProfiles.getState().profiles.bob?.bio).toBe("fresh");
  } finally {
    finish?.(profile("bob", "retired"));
    vi.useRealTimers();
  }
});
it("deduplicates profile reads and refreshes only observed accounts on events", async () => {
  api.get
    .mockResolvedValueOnce(profile("bob", "before"))
    .mockResolvedValueOnce(profile("bob", "after"));
  const first = useAccountProfiles.getState().load("bob");
  const second = useAccountProfiles.getState().load("bob");
  await Promise.all([first, second]);
  expect(api.get).toHaveBeenCalledTimes(1);
  await useAccountProfiles.getState().invalidate("unknown");
  expect(api.get).toHaveBeenCalledTimes(1);
  await useAccountProfiles.getState().invalidate("bob");
  expect(useAccountProfiles.getState().profiles.bob?.bio).toBe("after");
});
it("drops pending reads after session replacement and newer live refresh wins", async () => {
  let finish!: (value: ReturnType<typeof profile>) => void;
  api.get
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValueOnce(profile("bob", "new"));
  const pending = useAccountProfiles.getState().load("bob");
  await useAccountProfiles.getState().invalidate("bob");
  finish(profile("bob", "stale"));
  await pending;
  expect(useAccountProfiles.getState().profiles.bob?.bio).toBe("new");
  useAuth.setState({ generation: 2 });
  expect(useAccountProfiles.getState().profiles).toEqual({});
});

it("reloads a mounted observer after runtime startup even if its first read failed", async () => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  api.get
    .mockRejectedValueOnce(new Error("Node not ready"))
    .mockResolvedValueOnce(profile("bob", "Ready bio"));
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  function Observer() {
    const current = useAccountProfile("bob");
    return createElement("span", null, current?.bio ?? "");
  }
  try {
    await act(async () => root.render(createElement(Observer)));
    expect(host.textContent).toBe("");
    await act(async () => useAccountProfiles.getState().clear());
    expect(host.textContent).toBe("Ready bio");
  } finally {
    await act(async () => root.unmount());
    host.remove();
  }
});
it("retries a previously requested failed profile on a later live event without loading unobserved peers", async () => {
  api.get
    .mockRejectedValueOnce(new Error("Temporary read failure"))
    .mockResolvedValueOnce(profile("bob", "Recovered bio"));
  await expect(useAccountProfiles.getState().load("bob")).rejects.toThrow(
    "Temporary read failure",
  );
  await useAccountProfiles.getState().invalidate("unobserved");
  expect(api.get).toHaveBeenCalledTimes(1);
  await useAccountProfiles.getState().invalidate("bob");
  expect(api.get).toHaveBeenCalledTimes(2);
  expect(useAccountProfiles.getState().profiles.bob?.bio).toBe("Recovered bio");
});
