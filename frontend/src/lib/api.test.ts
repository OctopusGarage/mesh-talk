import { describe, it, expect, vi, beforeEach } from "vitest";

// The IPC contract: the frontend must call the exact Tauri command names with the exact
// (camelCase) arg keys the Rust commands expect. A typo here is a real runtime bug.
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { auth, chat } from "./api";

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
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
