// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
const probes = vi.hoisted(() => ({
  tasks: [] as Array<{
    decode: ReturnType<typeof deferred>;
    commit: ReturnType<typeof deferred>;
    pending: Promise<void>;
    settled: boolean;
    committing: boolean;
    wallpaper: string | null;
  }>,
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: probes.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));
vi.mock("@/features/chat/ChatApp", () => ({ ChatApp: () => null }));
vi.mock("@/features/auth/LoginScreen", () => ({ LoginScreen: () => null }));
vi.mock("@/features/auth/SensitiveVerificationDialog", () => ({
  SensitiveVerificationDialog: () => null,
}));
vi.mock("@/components/WindowControls", () => ({ WindowControls: () => null }));
vi.mock("@/lib/linkedAppearance", () => ({
  startLinkedAppearance: () => () => {},
}));
vi.mock("@/lib/initialPresentation", async (original) => {
  const actual = await original<typeof import("@/lib/initialPresentation")>();
  return {
    ...actual,
    prepareInitialPresentation: (
      ...args: Parameters<typeof actual.prepareInitialPresentation>
    ) => {
      const decode = deferred(),
        commit = deferred();
      const task = {
        wallpaper: args[0],
        decode,
        commit,
        pending: Promise.resolve(),
        settled: false,
        committing: false,
      };
      task.pending = actual
        .prepareInitialPresentation(args[0], args[1], {
          ...args[2],
          decode: () => decode.promise,
          commit: () => {
            task.committing = true;
            return commit.promise;
          },
        })
        .finally(() => {
          task.settled = true;
        });
      probes.tasks.push(task);
      return task.pending;
    },
  };
});
import App from "./App";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { usePacks } from "@/store/packs";
import { useTheme } from "@/lib/theme";
import { useAvatars } from "@/store/avatars";
import { useSettings } from "@/store/settings";
import { useAppLock } from "@/store/appLock";
let mounted = false;
let root: Root;
let host: HTMLDivElement;
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.useFakeTimers();
  probes.tasks = [];
  probes.invoke.mockReset();
  probes.invoke.mockResolvedValue(null);
  useAuth.setState({
    user: { id: "alice", username: "alice", display_name: "Alice" },
    generation: 7,
    booting: false,
    loggingOut: false,
    tryAutoLogin: vi.fn(async () => {}),
  });
  useChat.setState({
    myAccountId: "account",
    unreadOwner: "alice",
    runEpoch: 3,
    identityEpoch: 4,
    start: () => () => {},
  });
  usePacks.setState({
    scope: { owner: "alice", account: "account", session: "7:3" },
    loaded: true,
    packs: [
      {
        format: 1,
        id: "fixture-theme",
        version: "1.0.0",
        name: "Fixture",
        kind: "theme",
        base: "light",
        colors: {},
        wallpaper: "fixture:wallpaper",
      },
      {
        format: 1,
        id: "replacement-theme",
        version: "1.0.0",
        name: "Replacement",
        kind: "theme",
        base: "dark",
        colors: {},
        wallpaper: "fixture:replacement",
      },
    ],
  });
  useTheme.setState({
    theme: "fixture-theme",
    wallpaperEnabled: true,
    packWallpaperIds: {},
  });
  useAvatars.setState({ setOwner: () => {}, load: async () => true });
  useSettings.setState({
    setOwner: () => {},
    privacy: { ...useSettings.getState().privacy, auto_lock_seconds: 0 },
  });
  useAppLock.setState({ locked: false, refresh: async () => {} });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  mounted = true;
  await act(async () => root.render(createElement(App)));
  expect(probes.tasks).toHaveLength(1);
  expect(probes.tasks[0].wallpaper).toBe("fixture:wallpaper");
});
afterEach(async () => {
  if (mounted) await act(async () => root.unmount());
  mounted = false;
  await act(async () => {
    for (const task of probes.tasks) {
      task.decode.resolve();
      task.commit.resolve();
    }
    await Promise.all(probes.tasks.map((task) => task.pending));
  });
  host.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
const acknowledgements = () =>
  probes.invoke.mock.calls.filter(
    ([command]) => command === "initial_presentation_ready",
  );
it.each(["decode", "commit"] as const)(
  "App retires its actual presentation during %s before acknowledging",
  async (phase) => {
    const old = probes.tasks[0];
    if (phase === "commit") {
      await act(async () => old.decode.resolve());
      expect(old.committing).toBe(true);
    }
    await act(async () => useAuth.setState({ generation: 8 }));
    await act(async () => vi.advanceTimersByTimeAsync(100));
    expect(acknowledgements()).toHaveLength(0);
    expect(old.settled).toBe(false);
    await act(async () => {
      old.decode.resolve();
      old.commit.resolve();
      await old.pending;
    });
    expect(old.settled).toBe(true);
    expect(acknowledgements()).toHaveLength(0);
    expect(useAuth.getState().generation).toBe(8);
  },
);
it("App replacement presentation can finish before the retired decode without adopting its acknowledgement", async () => {
  const old = probes.tasks[0];
  await act(async () => useTheme.setState({ theme: "replacement-theme" }));
  expect(probes.tasks).toHaveLength(2);
  const replacement = probes.tasks[1];
  expect(replacement.wallpaper).toBe("fixture:replacement");
  await act(async () => {
    replacement.decode.resolve();
    replacement.commit.resolve();
    await replacement.pending;
  });
  expect(acknowledgements()).toHaveLength(1);
  expect(old.settled).toBe(false);
  await act(async () => {
    old.decode.resolve();
    old.commit.resolve();
    await old.pending;
  });
  expect(acknowledgements()).toHaveLength(1);
  expect(useTheme.getState().theme).toBe("replacement-theme");
});
it("App acknowledges the current presentation only after both real task phases settle", async () => {
  const task = probes.tasks[0];
  await act(async () => task.decode.resolve());
  expect(task.committing).toBe(true);
  expect(acknowledgements()).toHaveLength(0);
  await act(async () => {
    task.commit.resolve();
    await task.pending;
  });
  expect(acknowledgements()).toHaveLength(1);
});

it("App retires the actual effect on unmount before the held task completes", async () => {
  const old = probes.tasks[0];
  await act(async () => root.unmount());
  mounted = false;
  expect(old.settled).toBe(false);
  await act(async () => {
    old.decode.resolve();
    old.commit.resolve();
    await old.pending;
  });
  expect(old.settled).toBe(true);
  expect(acknowledgements()).toHaveLength(0);
});
