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

it("an older avatar load cannot overwrite a newer local edit", async () => {
  let finishLoad!: (value: Record<string, string>) => void;
  invoke.mockImplementation((command: string) =>
    command === "get_avatars"
      ? new Promise((resolve) => {
          finishLoad = resolve;
        })
      : Promise.resolve(),
  );
  const loading = useAvatars.getState().load();
  await useAvatars.getState().setAvatar("own", "new-photo");
  finishLoad({ own: "old-photo" });
  await loading;

  expect(useAvatars.getState().local.own).toBe("new-photo");
});

it("a load during an unfinished avatar write keeps the optimistic image", async () => {
  let finishWrite!: () => void;
  invoke.mockImplementation((command: string) => {
    if (command === "set_avatar")
      return new Promise<void>((resolve) => {
        finishWrite = resolve;
      });
    if (command === "get_avatars") return Promise.resolve({ own: "old-photo" });
    return Promise.resolve();
  });
  const saving = useAvatars.getState().setAvatar("own", "new-photo");
  await vi.waitFor(() => expect(finishWrite).toBeDefined());
  await useAvatars.getState().load();
  expect(useAvatars.getState().local.own).toBe("new-photo");
  finishWrite();
  await saving;
});

it("rapid avatar changes persist and publish the latest image last", async () => {
  let finishFirst!: () => void;
  const persisted: Record<string, string> = {};
  const published: string[] = [];
  let writes = 0;
  invoke.mockImplementation(
    (
      command: string,
      args?: { id?: string; dataUrl?: string; avatar?: string },
    ) => {
      if (command === "set_avatar") {
        writes++;
        if (writes === 1)
          return new Promise<void>((resolve) => {
            finishFirst = () => {
              persisted[args!.id!] = args!.dataUrl!;
              resolve();
            };
          });
        persisted[args!.id!] = args!.dataUrl!;
      }
      if (command === "publish_avatar") published.push(args!.avatar!);
      return Promise.resolve();
    },
  );

  const first = useAvatars.getState().setAvatar("own", "old-photo");
  const second = useAvatars.getState().setAvatar("own", "new-photo");
  await vi.waitFor(() => expect(finishFirst).toBeDefined());
  finishFirst();
  await Promise.all([first, second]);

  expect(persisted.own).toBe("new-photo");
  expect(published[published.length - 1]).toBe("new-photo");
});

it("a failed earlier save cannot roll back a repeated newer edit", async () => {
  let writes = 0;
  invoke.mockImplementation((command: string) => {
    if (command === "set_avatar" && ++writes === 1)
      return Promise.reject(new Error("disk error"));
    if (command === "get_avatars") return Promise.resolve({ own: "old-photo" });
    return Promise.resolve();
  });

  await Promise.all([
    useAvatars.getState().setAvatar("own", "new-photo"),
    useAvatars.getState().setAvatar("own", "new-photo"),
  ]);

  expect(useAvatars.getState().local.own).toBe("new-photo");
});

it("a pending avatar edit from the old session does not leak into the next session", async () => {
  let finishWrite!: () => void;
  invoke.mockImplementation((command: string) => {
    if (command === "set_avatar")
      return new Promise<void>((resolve) => {
        finishWrite = resolve;
      });
    if (command === "get_avatars") return Promise.resolve({});
    return Promise.resolve();
  });
  const oldSave = useAvatars.getState().setAvatar("old-own", "private-photo");
  await vi.waitFor(() => expect(finishWrite).toBeDefined());
  useAuth.setState({
    generation: 1,
    user: { id: "new", username: "new", display_name: "New" },
  });
  useAvatars.setState({ local: {}, ownId: "new-own" });

  await useAvatars.getState().load();

  expect(useAvatars.getState().local).toEqual({});
  finishWrite();
  await oldSave;
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
      await vi.waitFor(() => expect(finish).toBeDefined());
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
