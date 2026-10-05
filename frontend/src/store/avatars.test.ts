import { beforeEach, expect, it, vi } from "vitest";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@/lib/events", () => ({ subscribeNodeEvents: () => () => {} }));
import { useAvatars } from "./avatars";
import { useAuth } from "./auth";
import { useChat } from "./chat";

beforeEach(() => {
  invoke.mockReset();
  useAuth.setState({
    user: { id: "host", username: "a", display_name: "A" },
    generation: 0,
  });
  useChat.setState({ runEpoch: 0, identityEpoch: 0, ready: true });
  useAvatars.setState({ ownId: "own", local: {}, received: {} });
});

it("current own-avatar success publishes and current failure reconciles", async () => {
  invoke.mockResolvedValue(undefined);
  await useAvatars.getState().setAvatar("own", "photo");
  expect(invoke.mock.calls.map(([command]) => command)).toEqual([
    "set_avatar",
    "publish_avatar",
  ]);
  invoke.mockReset();
  invoke.mockImplementation((command: string) =>
    command === "set_avatar"
      ? Promise.reject(new Error("disk"))
      : Promise.resolve({ own: "persisted" }),
  );
  await useAvatars.getState().setAvatar("own", "failed");
  expect(invoke.mock.calls.map(([command]) => command)).toEqual([
    "set_avatar",
    "get_avatars",
  ]);
  expect(useAvatars.getState().local).toEqual({ own: "persisted" });
});

for (const action of ["load", "loadPeers", "reassertOwn"] as const) {
  it(`default ${action} cannot write or publish after a runtime replacement`, async () => {
    let finish!: (value: Record<string, string>) => void;
    invoke.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    const pending = useAvatars.getState()[action]();
    useChat.setState({ identityEpoch: 1 });
    useAvatars.setState({ local: { own: "new" }, received: { peer: "new" } });
    finish({ own: "old", peer: "old" });
    await pending;
    expect(useAvatars.getState().local).toEqual({ own: "new" });
    expect(useAvatars.getState().received).toEqual({ peer: "new" });
    expect(invoke).toHaveBeenCalledTimes(1);
  });
}

it("an explicit callback cannot override an invalid owned lease", async () => {
  let finish!: (value: Record<string, string>) => void;
  invoke.mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = useAvatars.getState().load(() => true);
  useAuth.setState({ generation: 1 });
  useAvatars.setState({ local: { new: "new" } });
  finish({ old: "old" });
  await pending;
  expect(useAvatars.getState().local).toEqual({ new: "new" });
});

for (const boundary of ["generation", "runEpoch", "identityEpoch"] as const) {
  for (const failed of [false, true]) {
    it(`ignores old avatar ${failed ? "failure" : "success"} after ${boundary}`, async () => {
      let finish!: () => void;
      invoke.mockImplementation((command: string) =>
        command === "set_avatar"
          ? new Promise<void>((resolve, reject) => {
              finish = () => (failed ? reject(new Error("old")) : resolve());
            })
          : Promise.resolve({ other: "new" }),
      );
      const pending = useAvatars.getState().setAvatar("own", "old");
      if (boundary === "generation") useAuth.setState({ generation: 1 });
      else useChat.setState({ [boundary]: 1 });
      useAvatars.setState({ local: { other: "new" }, ownId: "own" });
      finish();
      await pending;
      expect(invoke.mock.calls.map(([command]) => command)).toEqual([
        "set_avatar",
      ]);
      expect(useAvatars.getState().local).toEqual({ other: "new" });
    });
  }
}
