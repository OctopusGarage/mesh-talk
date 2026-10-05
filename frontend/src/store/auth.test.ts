import { describe, it, expect, vi, beforeEach } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { useAuth } from "./auth";

beforeEach(() => {
  invoke.mockReset();
  useAuth.setState({
    user: null,
    generation: 0,
    operation: 0,
    booting: true,
    loading: false,
    error: null,
  });
});

describe("auth store", () => {
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
    invoke.mockRejectedValueOnce("boom");
    await useAuth.getState().logout();
    expect(useAuth.getState().user).toBeNull();
  });
});
