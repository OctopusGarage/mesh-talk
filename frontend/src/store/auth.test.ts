import { describe, it, expect, vi, beforeEach } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { useAuth } from "./auth";

beforeEach(() => {
  invoke.mockReset();
  useAuth.setState({
    createdUsername: null,
    newAccount: false,
    savedSession: null,
    user: null,
    generation: 0,
    operation: 0,
    booting: true,
    loggingOut: false,
    loading: false,
    error: null,
  });
});

describe("auth store", () => {
  it("keeps the login screen gated until native logout finishes", async () => {
    let finishLogout!: () => void;
    invoke.mockImplementation((cmd: string) =>
      cmd === "logout"
        ? new Promise<void>((resolve) => {
            finishLogout = resolve;
          })
        : Promise.resolve(),
    );
    useAuth.setState({
      user: { id: "u1", username: "alice", display_name: "Alice" },
      booting: false,
    });
    const pending = useAuth.getState().logout();
    expect(useAuth.getState().user).toBeNull();
    expect(useAuth.getState().loggingOut).toBe(true);
    finishLogout();
    await pending;
    expect(useAuth.getState().loggingOut).toBe(false);
  });

  it("an old logout cannot release a newer session's logout gate", async () => {
    const finishes: Array<() => void> = [];
    invoke.mockImplementation((cmd: string) =>
      cmd === "logout"
        ? new Promise<void>((resolve) => finishes.push(resolve))
        : Promise.resolve({
            success: true,
            user: { id: "same", username: "alice", display_name: "Alice" },
          }),
    );
    const oldLogout = useAuth.getState().logout();
    await useAuth.getState().login("alice", "pw");
    const newLogout = useAuth.getState().logout();
    finishes[0]();
    await oldLogout;
    expect(useAuth.getState().loggingOut).toBe(true);
    finishes[1]();
    await newLogout;
    expect(useAuth.getState().loggingOut).toBe(false);
  });

  it("clears the OS unread badge when a session logs out", async () => {
    invoke.mockResolvedValue(undefined);
    useAuth.setState({
      user: { id: "u1", username: "alice", display_name: "Alice" },
    });

    await useAuth.getState().logout();

    expect(invoke).toHaveBeenCalledWith("set_badge", {
      count: 0,
      owner: "u1",
      revision: expect.any(Number),
    });
  });

  it("late same-UUID old-session catch cannot change the new session loading/error", async () => {
    let rejectOld!: (error: unknown) => void;
    invoke.mockImplementation((cmd: string) =>
      cmd === "rename_account"
        ? new Promise((_, reject) => {
            rejectOld = reject;
          })
        : cmd === "login"
          ? Promise.resolve({
              success: true,
              user: { id: "same", username: "alice", display_name: "new" },
            })
          : Promise.resolve({ success: true }),
    );
    useAuth.setState({
      user: { id: "same", username: "alice", display_name: "old" },
    });
    const rename = useAuth.getState().rename("stale");
    await useAuth.getState().logout();
    await useAuth.getState().login("alice", "pw");
    rejectOld("old error");
    await rename;
    expect(useAuth.getState()).toMatchObject({
      user: { id: "same", display_name: "new" },
      loading: false,
      error: null,
    });
  });

  it("invalidates locally at logout entry and cannot clear a later same-owner login", async () => {
    let finishLogout!: () => void;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "logout")
        return new Promise<void>((resolve) => {
          finishLogout = resolve;
        });
      if (cmd === "login")
        return Promise.resolve({
          success: true,
          user: { id: "u1", username: "alice", display_name: "new session" },
        });
      return Promise.resolve();
    });
    useAuth.setState({
      user: { id: "u1", username: "alice", display_name: "old" },
    });
    const logout = useAuth.getState().logout();
    expect(useAuth.getState().user).toBeNull();
    await useAuth.getState().login("alice", "pw");
    finishLogout();
    await logout;
    expect(useAuth.getState().user?.display_name).toBe("new session");
    expect(useAuth.getState().loggingOut).toBe(false);
    expect(
      invoke.mock.calls.some(([cmd]) => cmd === "clear_saved_session"),
    ).toBe(false);
  });

  it("rejects late automatic login after a manual session replaced it", async () => {
    let finishAuto!: (user: unknown) => void;
    invoke.mockImplementation((cmd: string) =>
      cmd === "auto_login"
        ? new Promise((resolve) => {
            finishAuto = resolve;
          })
        : Promise.resolve({
            success: true,
            user: { id: "b", username: "bob", display_name: "Bob" },
          }),
    );
    const auto = useAuth.getState().tryAutoLogin();
    await useAuth.getState().login("bob", "pw");
    finishAuto({ id: "a", username: "alice", display_name: "Alice" });
    await auto;
    expect(useAuth.getState().user?.id).toBe("b");
  });

  it("only the latest same-generation rename may publish", async () => {
    const finishes: Array<(user: unknown) => void> = [];
    useAuth.setState({
      user: { id: "u1", username: "alice", display_name: "old" },
    });
    invoke.mockImplementation(
      () =>
        new Promise((resolve) => {
          finishes.push(resolve);
        }),
    );
    const first = useAuth.getState().rename("First");
    const second = useAuth.getState().rename("Second");
    finishes[1]({ id: "u1", username: "alice", display_name: "Second" });
    await second;
    finishes[0]({ id: "u1", username: "alice", display_name: "First" });
    await first;
    expect(useAuth.getState().user?.display_name).toBe("Second");
  });

  it("login stores the user on success", async () => {
    invoke.mockResolvedValueOnce({
      success: true,
      user: { id: "u1", username: "alice", display_name: "alice" },
    });
    const ok = await useAuth.getState().login("alice", "pw");
    expect(ok).toBe(true);
    expect(useAuth.getState().user).toEqual({
      id: "u1",
      username: "alice",
      display_name: "alice",
    });
    expect(useAuth.getState().error).toBeNull();
  });

  it("login with success:false stays signed out + sets an error", async () => {
    invoke.mockResolvedValueOnce({ success: false });
    const ok = await useAuth.getState().login("a", "b");
    expect(ok).toBe(false);
    expect(useAuth.getState().user).toBeNull();
    expect(useAuth.getState().error).toBeTruthy();
  });

  it("login surfaces a rejected invoke (backend error string)", async () => {
    invoke.mockRejectedValueOnce("Invalid username or password");
    const ok = await useAuth.getState().login("a", "b");
    expect(ok).toBe(false);
    expect(useAuth.getState().error).toBe("Invalid username or password");
  });

  it("rename stores the updated user on success", async () => {
    useAuth.setState({
      user: { id: "u1", username: "alice", display_name: "alice" },
    });
    invoke.mockResolvedValueOnce({
      id: "u1",
      username: "alice",
      display_name: "Alice 🐻",
    });
    const ok = await useAuth.getState().rename("Alice 🐻");
    expect(ok).toBe(true);
    expect(useAuth.getState().user?.display_name).toBe("Alice 🐻");
    // The login username is untouched by a rename.
    expect(useAuth.getState().user?.username).toBe("alice");
  });

  it("rename surfaces a backend error and keeps the old name", async () => {
    useAuth.setState({
      user: { id: "u1", username: "alice", display_name: "alice" },
    });
    invoke.mockRejectedValueOnce("Invalid display name");
    const ok = await useAuth.getState().rename("");
    expect(ok).toBe(false);
    expect(useAuth.getState().user?.display_name).toBe("alice");
    expect(useAuth.getState().error).toBe("Invalid display name");
  });

  it("logout clears the session even if the backend errors", async () => {
    useAuth.setState({ user: { id: "u", username: "x", display_name: "x" } });
    invoke.mockImplementation((cmd: string) =>
      cmd === "logout" ? Promise.reject("boom") : Promise.resolve(),
    );
    await useAuth.getState().logout();
    expect(useAuth.getState().user).toBeNull();
    expect(useAuth.getState().loggingOut).toBe(false);
  });
});

it("creates an account and enters through login without another password entry", async () => {
  const user = { id: "new", username: "alice", display_name: "Alice" };
  invoke.mockResolvedValueOnce({ success: true, user });
  invoke.mockResolvedValueOnce({ success: true, user, saved_session: "saved" });
  expect(await useAuth.getState().register("alice", " raw password ")).toBe(
    true,
  );
  expect(invoke.mock.calls.map(([cmd]) => cmd)).toEqual(["register", "login"]);
  expect(invoke).toHaveBeenLastCalledWith("login", {
    username: "alice",
    password: " raw password ",
  });
  expect(useAuth.getState()).toMatchObject({
    user,
    newAccount: true,
    savedSession: "saved",
    loading: false,
  });
});

it("preserves account creation when entering fails and lets login retry independently", async () => {
  invoke.mockResolvedValueOnce({ success: true });
  invoke.mockRejectedValueOnce("Startup unavailable");
  expect(await useAuth.getState().register("alice", "password123")).toBe(false);
  expect(useAuth.getState()).toMatchObject({
    user: null,
    createdUsername: "alice",
    error: "Startup unavailable",
    loading: false,
  });
  invoke.mockResolvedValueOnce({
    success: true,
    user: { id: "new", username: "alice", display_name: "Alice" },
    saved_session: "failed",
  });
  expect(await useAuth.getState().login("alice", "password123")).toBe(true);
  expect(useAuth.getState()).toMatchObject({
    newAccount: true,
    savedSession: "failed",
    createdUsername: null,
  });
  expect(invoke.mock.calls.filter(([cmd]) => cmd === "register")).toHaveLength(
    1,
  );
});

it("does not begin login if registration finishes after logout", async () => {
  let finish!: (value: unknown) => void;
  invoke.mockImplementation((cmd: string) =>
    cmd === "register"
      ? new Promise((resolve) => {
          finish = resolve;
        })
      : Promise.resolve(),
  );
  const pending = useAuth.getState().register("alice", "password123");
  await useAuth.getState().logout();
  finish({ success: true });
  expect(await pending).toBe(false);
  expect(invoke.mock.calls.some(([cmd]) => cmd === "login")).toBe(false);
});

it("a saved-login retry cannot update a replacement session", async () => {
  let finish!: (status: unknown) => void;
  useAuth.setState({
    user: { id: "u1", username: "alice", display_name: "Alice" },
    savedSession: "failed",
  });
  invoke.mockImplementation((cmd: string) =>
    cmd === "retry_saved_session"
      ? new Promise((resolve) => {
          finish = resolve;
        })
      : Promise.resolve(),
  );
  const pending = useAuth.getState().retrySavedSession();
  await useAuth.getState().logout();
  useAuth.setState({
    user: { id: "u1", username: "alice", display_name: "new session" },
    savedSession: "disabled",
  });
  finish("saved");
  expect(await pending).toBe(false);
  expect(useAuth.getState().savedSession).toBe("disabled");
});

it("cannot retry saved credentials without a signed-in owner", async () => {
  expect(await useAuth.getState().retrySavedSession()).toBe(false);
  expect(invoke).not.toHaveBeenCalled();
});

it.each([
  { response: { success: false }, error: "Registration failed" },
  {
    rejection: "Identity store unavailable",
    error: "Identity store unavailable",
  },
])(
  "failed creation stays signed out and does not attempt login: $error",
  async ({ response, rejection, error }) => {
    if (rejection) invoke.mockRejectedValueOnce(rejection);
    else invoke.mockResolvedValueOnce(response);
    expect(await useAuth.getState().register("alice", "password123")).toBe(
      false,
    );
    expect(useAuth.getState()).toMatchObject({
      user: null,
      createdUsername: null,
      loading: false,
      error,
    });
    expect(invoke.mock.calls.map(([cmd]) => cmd)).toEqual(["register"]);
  },
);

describe("device enrollment session publication", () => {
  it("enters the kept account through the normal authenticated state without registering a temporary user", async () => {
    invoke.mockResolvedValue({
      success: true,
      token: "native-token",
      saved_session: "saved",
      user: {
        id: "retained-local-user",
        username: "kept-login",
        display_name: "Kept nickname",
      },
    });
    expect(
      await useAuth
        .getState()
        .enrollDevice("attempt", "verified-source", "a".repeat(32), true),
    ).toBe(true);
    expect(invoke).toHaveBeenCalledExactlyOnceWith(
      "complete_device_enrollment",
      {
        enrollmentId: "attempt",
        sourceId: "verified-source",
        code: "a".repeat(32),
        includeHistory: true,
      },
    );
    expect(useAuth.getState()).toMatchObject({
      user: {
        id: "retained-local-user",
        username: "kept-login",
        display_name: "Kept nickname",
      },
      loading: false,
      booting: false,
      newAccount: false,
      savedSession: "saved",
    });
  });

  it("does not publish a late enrollment response into a replacement authentication operation", async () => {
    let finish!: (value: unknown) => void;
    invoke.mockImplementation((command: string) =>
      command === "complete_device_enrollment"
        ? new Promise((resolve) => {
            finish = resolve;
          })
        : Promise.resolve({
            success: true,
            user: {
              id: "replacement",
              username: "other",
              display_name: "Other",
            },
          }),
    );
    const previous = useAuth
      .getState()
      .enrollDevice("old", "source", "a".repeat(32), false);
    await useAuth.getState().login("other", "password");
    finish({
      success: true,
      user: { id: "old", username: "old-login", display_name: "Old" },
    });
    expect(await previous).toBe(false);
    expect(useAuth.getState().user?.id).toBe("replacement");
  });
});
