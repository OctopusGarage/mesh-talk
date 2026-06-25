import { describe, it, expect, vi, beforeEach } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { useAuth } from "./auth";

beforeEach(() => {
  invoke.mockReset();
  useAuth.setState({ user: null, loading: false, error: null });
});

describe("auth store", () => {
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
