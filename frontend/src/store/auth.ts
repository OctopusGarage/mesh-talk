import { create } from "zustand";
import { auth, chat } from "@/lib/api";
import { errorMessage as errMsg } from "@/lib/error";
import type { UserInfo } from "@/lib/types";

interface AuthState {
  user: UserInfo | null;
  generation: number;
  operation: number;
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

export const useAuth = create<AuthState>((set, get) => ({
  user: null,
  generation: 0,
  operation: 0,
  booting: true,
  loading: false,
  error: null,
  clearError: () => set({ error: null }),

  tryAutoLogin: async () => {
    const generation = get().generation;
    const operation = get().operation + 1;
    set({ operation });
    try {
      const user = await auth.autoLogin();
      if (get().generation !== generation || get().operation !== operation)
        return;
      // Stale/absent secret resolves to null → fall through to the login screen.
      set({ user: user ?? null, booting: false });
    } catch {
      if (get().generation !== generation || get().operation !== operation)
        return;
      // Auto-login is best-effort; any failure just means manual login.
      set({ booting: false });
    }
  },

  login: async (username, password) => {
    const generation = get().generation + 1;
    const operation = get().operation + 1;
    set({ generation, operation, user: null, loading: true, error: null });
    try {
      const res = await auth.login(username, password);
      if (get().generation !== generation || get().operation !== operation)
        return false;
      if (!res.success || !res.user) throw new Error("Unable to sign in");
      set({ user: res.user, loading: false, booting: false });
      return true;
    } catch (e) {
      if (get().generation !== generation || get().operation !== operation)
        return false;
      set({ error: errMsg(e), loading: false });
      return false;
    }
  },

  register: async (username, password) => {
    const generation = get().generation;
    const operation = get().operation + 1;
    set({ operation, loading: true, error: null });
    try {
      const res = await auth.register(username, password);
      if (get().generation !== generation || get().operation !== operation)
        return false;
      if (!res.success) throw new Error("Registration failed");
      set({ loading: false });
      return true;
    } catch (e) {
      if (get().generation !== generation || get().operation !== operation)
        return false;
      set({ error: errMsg(e), loading: false });
      return false;
    }
  },

  rename: async (newDisplayName) => {
    const generation = get().generation;
    const owner = get().user?.id;
    if (!owner) return false;
    const operation = get().operation + 1;
    set({ operation, loading: true, error: null });
    try {
      const user = await auth.renameAccount(newDisplayName);
      if (
        get().generation !== generation ||
        get().operation !== operation ||
        get().user?.id !== owner
      )
        return false;
      set({ user, loading: false });
      return true;
    } catch (e) {
      if (
        get().generation !== generation ||
        get().operation !== operation ||
        get().user?.id !== owner
      )
        return false;
      set({ error: errMsg(e), loading: false });
      return false;
    }
  },

  logout: async () => {
    // Invalidate before native retirement awaits, including a same-UUID relogin.
    set((s) => ({
      user: null,
      generation: s.generation + 1,
      operation: s.operation + 1,
      loading: false,
      booting: false,
      error: null,
    }));
    // The unread badge belongs to the signed-in session. ChatApp unmounts at this
    // point, so its unread-count effect cannot clear the OS badge on its own.
    void chat.setBadge(0).catch(() => {});
    try {
      await auth.logout();
    } catch {
      // ignore — we clear the local session regardless
    }
    // Native logout already serializes exact-original-owner remembered-secret cleanup.
    // No ownerless post-retirement cleanup or state mutation may target a newer login.
  },
}));
