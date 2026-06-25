import { create } from "zustand";
import { auth } from "@/lib/api";
import { errorMessage as errMsg } from "@/lib/error";
import type { UserInfo } from "@/lib/types";

interface AuthState {
  user: UserInfo | null;
  /** True from app start until the one-shot auto-login attempt resolves; gates a brief
   * "unlocking…" splash so we don't flash the login screen for a remembered session. */
  booting: boolean;
  loading: boolean;
  error: string | null;
  clearError: () => void;
  /** One-shot on app start: resume a saved keychain session if "stay signed in" is on. */
  tryAutoLogin: () => Promise<void>;
  login: (username: string, password: string) => Promise<boolean>;
  register: (username: string, password: string) => Promise<boolean>;
  /** Change the editable display name (nickname). Returns true on success. */
  rename: (newDisplayName: string) => Promise<boolean>;
  logout: () => Promise<void>;
}

export const useAuth = create<AuthState>((set) => ({
  user: null,
  booting: true,
  loading: false,
  error: null,
  clearError: () => set({ error: null }),

  tryAutoLogin: async () => {
    try {
      const user = await auth.autoLogin();
      // Stale/absent secret resolves to null → fall through to the login screen.
      set({ user: user ?? null, booting: false });
    } catch {
      // Auto-login is best-effort; any failure just means manual login.
      set({ booting: false });
    }
  },

  login: async (username, password) => {
    set({ loading: true, error: null });
    try {
      const res = await auth.login(username, password);
      if (!res.success || !res.user) throw new Error("Unable to sign in");
      set({ user: res.user, loading: false });
      return true;
    } catch (e) {
      set({ error: errMsg(e), loading: false });
      return false;
    }
  },

  register: async (username, password) => {
    set({ loading: true, error: null });
    try {
      const res = await auth.register(username, password);
      if (!res.success) throw new Error("Registration failed");
      set({ loading: false });
      return true;
    } catch (e) {
      set({ error: errMsg(e), loading: false });
      return false;
    }
  },

  rename: async (newDisplayName) => {
    set({ loading: true, error: null });
    try {
      const user = await auth.renameAccount(newDisplayName);
      set({ user, loading: false });
      return true;
    } catch (e) {
      set({ error: errMsg(e), loading: false });
      return false;
    }
  },

  logout: async () => {
    try {
      await auth.logout();
    } catch {
      // ignore — we clear the local session regardless
    }
    // Explicit sign-out must forget the saved keychain session, otherwise the next
    // launch would silently auto-login the user back in. Best-effort.
    try {
      await auth.clearSavedSession();
    } catch {
      // ignore — degrade to leaving the (now unusable post-logout) secret in place
    }
    set({ user: null });
  },
}));
