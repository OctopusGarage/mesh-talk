import { describe, it, expect, vi, beforeEach } from "vitest";

// The IPC contract: the frontend must call the exact Tauri command names with the exact
// (camelCase) arg keys the Rust commands expect. A typo here is a real runtime bug.
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { auth, chat, contactPolicy, settings } from "./api";

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
});

describe("contact visibility command mapping", () => {
  it("binds reads and writes to the expected local owner", async () => {
    await contactPolicy.get("owner");
    expect(invoke).toHaveBeenCalledWith("get_hidden_contacts", {
      owner: "owner",
    });
    await contactPolicy.set("owner", "account", true, "Name");
    expect(invoke).toHaveBeenCalledWith("set_contact_hidden", {
      owner: "owner",
      account: "account",
      hidden: true,
      name: "Name",
    });
  });
});

describe("settings updates", () => {
  it("preserves both fields when two updates are requested before the first write finishes", async () => {
    const persisted = { calls_enabled: false, notifications: true };
    let releaseFirst!: () => void;
    let writes = 0;
    invoke.mockImplementation(
      (command: string, args?: { settings: typeof persisted }) => {
        if (command === "get_app_settings")
          return Promise.resolve({ ...persisted });
        if (command === "set_app_settings") {
          writes++;
          if (writes === 1)
            return new Promise<void>((resolve) => {
              releaseFirst = () => {
                Object.assign(persisted, args!.settings);
                resolve();
              };
            });
          Object.assign(persisted, args!.settings);
          return Promise.resolve();
        }
        return Promise.resolve();
      },
    );

    const first = settings.update({ calls_enabled: true });
    const second = settings.update({ notifications: false });
    await vi.waitFor(() => expect(releaseFirst).toBeDefined());
    releaseFirst();
    await Promise.all([first, second]);

    expect(persisted).toMatchObject({
      calls_enabled: true,
      notifications: false,
    });
  });

  it("continues applying later updates after an earlier write fails", async () => {
    const persisted = { notifications: true };
    let writes = 0;
    invoke.mockImplementation(
      (command: string, args?: { settings: typeof persisted }) => {
        if (command === "get_app_settings")
          return Promise.resolve({ ...persisted });
        if (command === "set_app_settings") {
          if (++writes === 1) return Promise.reject(new Error("disk full"));
          Object.assign(persisted, args!.settings);
        }
        return Promise.resolve();
      },
    );

    const failed = settings.update({ notifications: false });
    const later = settings.update({ notifications: false });
    await expect(failed).rejects.toThrow("disk full");
    await later;
    expect(persisted.notifications).toBe(false);
  });
});

describe("auth command mapping", () => {
  it("login", async () => {
    await auth.login("u", "p");
    expect(invoke).toHaveBeenCalledWith("login", {
      username: "u",
      password: "p",
    });
  });
  it("register", async () => {
    await auth.register("u", "p");
    expect(invoke).toHaveBeenCalledWith("register", {
      username: "u",
      password: "p",
    });
  });
  it("logout / adopt take no args", async () => {
    await auth.logout();
    expect(invoke).toHaveBeenCalledWith("logout");
    await auth.adoptLinkedAccount();
    expect(invoke).toHaveBeenCalledWith("adopt_linked_account");
  });
  it("auto_login / clear_saved_session take no args", async () => {
    await auth.autoLogin();
    expect(invoke).toHaveBeenCalledWith("auto_login");
    await auth.clearSavedSession();
    expect(invoke).toHaveBeenCalledWith("clear_saved_session");
  });
});

describe("chat command mapping", () => {
  it("my_id / account_id", async () => {
    await chat.myId();
    expect(invoke).toHaveBeenCalledWith("my_id");
    await chat.accountId();
    expect(invoke).toHaveBeenCalledWith("account_id");
  });
  it("send_dm maps replyTo (and defaults it to null)", async () => {
    await chat.sendDm("r", "hello", "ev1");
    expect(invoke).toHaveBeenCalledWith("send_dm", {
      recipient: "r",
      text: "hello",
      replyTo: "ev1",
    });
    await chat.sendDm("r", "hello");
    expect(invoke).toHaveBeenLastCalledWith("send_dm", {
      recipient: "r",
      text: "hello",
      replyTo: null,
    });
  });
  it("send_to_account", async () => {
    await chat.sendToAccount("acct", "hi", null);
    expect(invoke).toHaveBeenCalledWith("send_to_account", {
      account: "acct",
      text: "hi",
      replyTo: null,
    });
  });
  it("create_channel maps memberIds", async () => {
    await chat.createChannel("general", ["a", "b"]);
    expect(invoke).toHaveBeenCalledWith("create_channel", {
      name: "general",
      memberIds: ["a", "b"],
    });
  });
  it("react_channel maps channelId/target/emoji/remove", async () => {
    await chat.reactChannel("chan", "tgt", "👍", true);
    expect(invoke).toHaveBeenCalledWith("react_channel", {
      channelId: "chan",
      target: "tgt",
      emoji: "👍",
      remove: true,
    });
  });
  it("save_file maps fileConv/dest", async () => {
    await chat.saveFile("fc", "/tmp/out");
    expect(invoke).toHaveBeenCalledWith("save_file", {
      fileConv: "fc",
      dest: "/tmp/out",
    });
  });
  it("link_device maps peer/code", async () => {
    await chat.linkDevice("peer1", "CODE");
    expect(invoke).toHaveBeenCalledWith("link_device", {
      peer: "peer1",
      code: "CODE",
    });
  });
});
