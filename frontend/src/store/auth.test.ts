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
      user: { id: "u1", username: "alice" },
    });
    const ok = await useAuth.getState().login("alice", "pw");
    expect(ok).toBe(true);
    expect(useAuth.getState().user).toEqual({ id: "u1", username: "alice" });
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

  it("logout clears the session even if the backend errors", async () => {
    useAuth.setState({ user: { id: "u", username: "x" } });
    invoke.mockRejectedValueOnce("boom");
    await useAuth.getState().logout();
    expect(useAuth.getState().user).toBeNull();
  });
});
