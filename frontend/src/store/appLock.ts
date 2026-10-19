import { create } from "zustand";
import { auth } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { useCalls } from "@/store/calls";
import { useAvatars } from "@/store/avatars";
import { usePresence } from "@/store/presence";
import { useContactPolicy } from "@/store/contactPolicy";
import { usePrivacy } from "@/store/privacy";

interface AppLockState {
  locked: boolean;
  busy: boolean;
  error: string | null;
  lock: () => Promise<void>;
  refresh: () => Promise<void>;
  unlock: (password?: string) => Promise<void>;
}

function hideSensitiveState() {
  useAuth.setState((state) => ({ generation: state.generation + 1 }));
  useChat.getState().resetForLock();
  useAvatars.getState().setOwner(null);
  usePresence.getState().clear();
  useContactPolicy.getState().reset();
  usePrivacy.getState().reset();
  useCalls.getState().teardown();
}

export const useAppLock = create<AppLockState>((set, get) => ({
  locked: false,
  busy: false,
  error: null,
  lock: async () => {
    if (get().locked || !useAuth.getState().user) return;
    useCalls.getState().hangup();
    hideSensitiveState();
    set({ locked: true, busy: true, error: null });
    try {
      await auth.lock();
      set({ busy: false });
    } catch (error) {
      set({ locked: false, busy: false, error: errorMessage(error) });
    }
  },
  refresh: async () => {
    if (!useAuth.getState().user) return;
    try {
      if (await auth.lockStatus()) {
        if (!get().locked) hideSensitiveState();
        set({ locked: true, busy: false });
      }
    } catch {
      /* native bridge unavailable */
    }
  },
  unlock: async (password) => {
    set({ busy: true, error: null });
    try {
      await auth.unlock(password);
      set({ locked: false, busy: false });
    } catch (error) {
      set({ busy: false, error: errorMessage(error) });
    }
  },
}));
