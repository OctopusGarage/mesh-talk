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
  useAvatars.setState({ owner: "host", ownId: "own", local: {}, received: {} });
});

it("own-avatar publication carries the originating owner and cryptographic account", async () => {
  invoke.mockResolvedValue(undefined);
  await useAvatars.getState().setAvatar("own", "scoped-photo");
  expect(invoke).toHaveBeenCalledWith("publish_avatar", {
    owner: "host",
    expectedAccount: "own",
    avatar: "scoped-photo",
  });
});

it("clears local and received avatars when the signed-in owner changes", () => {
  useAvatars.setState({
    local: { contact: "alice-local" },
    received: { contact: "alice-peer" },
  });
  useAvatars.getState().setOwner("bob");
  expect(useAvatars.getState().local).toEqual({});
  expect(useAvatars.getState().received).toEqual({});
  expect(useAvatars.getState().ownId).toBe("");
});

it("a linked device's signed own-avatar update replaces the stale local override", async () => {
  invoke.mockResolvedValue(undefined);
  useAvatars.setState({ local: { own: "old-local" } });
  useAvatars.getState().mergeReceived("own", "new-linked-photo");
  expect(useAvatars.getState().local.own).toBeUndefined();
  expect(useAvatars.getState().received.own).toBe("new-linked-photo");
  await vi.waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("set_avatar", {
      expectedAccount: "own",
      owner: "host",
      id: "own",
      dataUrl: null,
    }),
  );
  expect(
    invoke.mock.calls.some(([command]) => command === "publish_avatar"),
  ).toBe(false);
});

it("a linked device's avatar clear removes both own-avatar sources", async () => {
  invoke.mockResolvedValue(undefined);
  useAvatars.setState({
    local: { own: "old-local" },
    received: { own: "remote" },
  });
  useAvatars.getState().mergeReceived("own", null);
  expect(useAvatars.getState().local.own).toBeUndefined();
  expect(useAvatars.getState().received.own).toBeUndefined();
  await vi.waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("set_avatar", {
      expectedAccount: "own",
      owner: "host",
      id: "own",
      dataUrl: null,
    }),
  );
});

it("a durable linked-device clear prevents a stale avatar from being republished on restart", async () => {
  invoke.mockImplementation((command: string) => {
    if (command === "peer_profile_avatar")
      return Promise.resolve({ account_id: "own", avatar: null });
    if (command === "get_avatars") return Promise.resolve({ own: "old-local" });
    return Promise.resolve();
  });
  await useAvatars.getState().reassertOwn();
  expect(useAvatars.getState().local.own).toBeUndefined();
  expect(useAvatars.getState().received.own).toBeUndefined();
  expect(
    invoke.mock.calls.some(([command]) => command === "publish_avatar"),
  ).toBe(false);
  await vi.waitFor(() =>
    expect(invoke).toHaveBeenCalledWith("set_avatar", {
      expectedAccount: "own",
      owner: "host",
      id: "own",
      dataUrl: null,
    }),
  );
});

it("a stale startup profile load cannot replace a newer live update", async () => {
  let finishPeers!: (
    value: { account_id: string; avatar: string | null } | null,
  ) => void;
  invoke.mockImplementation((command: string) =>
    command === "peer_profile_avatar"
      ? new Promise((resolve) => {
          finishPeers = resolve;
        })
      : Promise.resolve(),
  );
  const loading = useAvatars.getState().loadPeers();
  useAvatars.getState().mergeReceived("contact", "new-photo");
  finishPeers({ account_id: "contact", avatar: "old-photo" });
  await loading;
  expect(useAvatars.getState().received.contact).toBe("new-photo");
});

it("clearing my own avatar removes the previously received sibling photo immediately", async () => {
  invoke.mockResolvedValue(undefined);
  useAvatars.setState({
    received: { own: "sibling-photo" },
    ownProfileSeen: true,
  });
  await useAvatars.getState().setAvatar("own", null);
  expect(useAvatars.getState().received.own).toBeUndefined();
  expect(useAvatars.getState().local.own).toBeUndefined();
  expect(invoke).toHaveBeenCalledWith("publish_avatar", {
    owner: "host",
    expectedAccount: "own",
    avatar: null,
  });
});

it("startup never republishes an older avatar after a newer local edit", async () => {
  let finishPeers!: (
    value: { account_id: string; avatar: string | null } | null,
  ) => void;
  invoke.mockImplementation((command: string) => {
    if (command === "get_avatars") return Promise.resolve({ own: "old-photo" });
    if (command === "peer_profile_avatar")
      return new Promise((resolve) => {
        finishPeers = resolve;
      });
    return Promise.resolve();
  });
  const reassert = useAvatars.getState().reassertOwn();
  await vi.waitFor(() => expect(finishPeers).toBeDefined());
  await useAvatars.getState().setAvatar("own", "new-photo");
  finishPeers(null);
  await reassert;
  expect(
    invoke.mock.calls.filter(([command]) => command === "publish_avatar"),
  ).toEqual([
    [
      "publish_avatar",
      { owner: "host", expectedAccount: "own", avatar: "new-photo" },
    ],
  ]);
});

it("startup skips republish when the durable profile load fails", async () => {
  invoke.mockImplementation((command: string) => {
    if (command === "get_avatars") return Promise.resolve({ own: "old-photo" });
    if (command === "peer_profile_avatar")
      return Promise.reject(new Error("unavailable"));
    return Promise.resolve();
  });
  await useAvatars.getState().reassertOwn();
  expect(
    invoke.mock.calls.some(([command]) => command === "publish_avatar"),
  ).toBe(false);
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
  await expect(
    useAvatars.getState().setAvatar("own", "failed"),
  ).rejects.toThrow();
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

it("a load during an unfinished avatar clear does not restore the old image", async () => {
  let finishWrite!: () => void;
  invoke.mockImplementation((command: string) => {
    if (command === "set_avatar")
      return new Promise<void>((resolve) => {
        finishWrite = resolve;
      });
    if (command === "get_avatars") return Promise.resolve({ own: "old-photo" });
    return Promise.resolve();
  });
  useAvatars.setState({ local: { own: "old-photo" } });
  const saving = useAvatars.getState().setAvatar("own", null);
  await vi.waitFor(() => expect(finishWrite).toBeDefined());
  await useAvatars.getState().load();
  expect(useAvatars.getState().local.own).toBeUndefined();
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

it("a queued edit from a retired session never writes to the next session", async () => {
  let finishFirst!: () => void;
  invoke.mockImplementation((command: string) => {
    if (command === "set_avatar")
      return new Promise<void>((resolve) => {
        finishFirst = resolve;
      });
    return Promise.resolve();
  });
  const first = useAvatars.getState().setAvatar("first", "old-photo");
  await vi.waitFor(() => expect(finishFirst).toBeDefined());
  const queued = useAvatars.getState().setAvatar("second", "private-photo");
  useAuth.setState({ generation: 1 });
  finishFirst();
  await Promise.all([first, queued]);
  expect(
    invoke.mock.calls.filter(([command]) => command === "set_avatar"),
  ).toHaveLength(1);
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

for (const avatar of ["new-photo", null]) {
  it(`rejects a peer snapshot started during an own-avatar ${avatar ? "edit" : "clear"} that returns after publication`, async () => {
    let finishPublish!: () => void;
    let finishPeers!: (
      profiles: { account_id: string; avatar: string | null } | null,
    ) => void;
    const persisted: Record<string, string> = { own: "old-photo" };
    invoke.mockImplementation(
      (command: string, args?: { id?: string; dataUrl?: string | null }) => {
        if (command === "set_avatar") {
          if (args?.dataUrl) persisted[args.id!] = args.dataUrl;
          else delete persisted[args!.id!];
        }
        if (command === "publish_avatar")
          return new Promise<void>((resolve) => {
            finishPublish = resolve;
          });
        if (command === "peer_profile_avatar")
          return new Promise<{
            account_id: string;
            avatar: string | null;
          } | null>((resolve) => {
            finishPeers = resolve;
          });
        return Promise.resolve();
      },
    );
    useAvatars.setState({
      local: { own: "old-photo" },
      received: {},
      ownProfileSeen: false,
    });
    const saving = useAvatars.getState().setAvatar("own", avatar);
    await vi.waitFor(() => expect(finishPublish).toBeDefined());
    const loading = useAvatars.getState().loadPeers();
    finishPublish();
    await saving;
    finishPeers({ account_id: "own", avatar: "old-photo" });
    expect(await loading).toBe(false);
    expect(useAvatars.getState().local.own).toBe(avatar ?? undefined);
    expect(useAvatars.getState().received.own).toBeUndefined();
    expect(persisted.own).toBe(avatar ?? undefined);
    expect(
      invoke.mock.calls.filter(([command]) => command === "publish_avatar"),
    ).toEqual([
      ["publish_avatar", { owner: "host", expectedAccount: "own", avatar }],
    ]);
  });
}

it("rejects a current avatar CAS failure after restoring the authoritative saved picture", async () => {
  invoke.mockImplementation((command: string) => {
    if (command === "set_avatar")
      return Promise.reject(new Error("CAS conflict"));
    if (command === "get_avatars")
      return Promise.resolve({ contact: "sibling-photo" });
    return Promise.resolve();
  });
  await expect(
    useAvatars.getState().setAvatar("contact", "chosen-photo"),
  ).rejects.toThrow("CAS conflict");
  expect(useAvatars.getState().local.contact).toBe("sibling-photo");
  invoke.mockResolvedValue(undefined);
  await useAvatars.getState().setAvatar("contact", "retry-photo");
  expect(useAvatars.getState().local.contact).toBe("retry-photo");
});

it("does not report an older avatar failure after a newer intent starts during rollback", async () => {
  let read!: (value: Record<string, string>) => void;
  let writes = 0;
  invoke.mockImplementation((command: string) => {
    if (command === "set_avatar" && ++writes === 1)
      return Promise.reject(new Error("first CAS conflict"));
    if (command === "get_avatars")
      return new Promise((resolve) => {
        read = resolve;
      });
    return Promise.resolve();
  });
  const first = useAvatars.getState().setAvatar("contact", "first");
  await vi.waitFor(() => expect(read).toBeDefined());
  const second = useAvatars.getState().setAvatar("contact", "second");
  read({ contact: "saved" });
  await Promise.all([first, second]);
  expect(useAvatars.getState().local.contact).toBe("second");
});

it("bounds completed current-profile pages while retaining our signed avatar", async () => {
  invoke.mockResolvedValue(undefined);
  useAvatars.getState().mergeReceived("own", "own-photo");
  for (let index = 0; index < 1200; index++)
    useAvatars.getState().mergeReceived(`peer-${index}`, `photo-${index}`);
  const received = useAvatars.getState().received;
  expect(Object.keys(received)).toHaveLength(1024);
  expect(received.own).toBe("own-photo");
  expect(received["peer-1199"]).toBe("photo-1199");
  expect(received["peer-0"]).toBeUndefined();
});

it("bounds avatar strings by bytes as well as profile count", () => {
  const photo = "x".repeat(128 * 1024);
  for (let index = 0; index < 300; index++)
    useAvatars.getState().mergeReceived(`peer-${index}`, photo);
  const received = useAvatars.getState().received;
  expect(
    Object.values(received).reduce((sum, value) => sum + value.length * 2, 0),
  ).toBeLessThanOrEqual(64 * 1024 * 1024);
  expect(received["peer-299"]).toBe(photo);
});
