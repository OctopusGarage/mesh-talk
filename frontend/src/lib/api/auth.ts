import { invoke } from "@tauri-apps/api/core";
import type {
  LoginResult,
  LogoutResult,
  RegisterResult,
  UserInfo,
} from "../types";

export const auth = {
  login: (username: string, password: string) =>
    invoke<LoginResult>("login", { username, password }),
  register: (username: string, password: string) =>
    invoke<RegisterResult>("register", { username, password }),
  /** Change the editable display name (nickname); returns the updated user. */
  renameAccount: (newDisplayName: string) =>
    invoke<UserInfo>("rename_account", { newDisplayName }),
  logout: () => invoke<LogoutResult>("logout"),
  adoptLinkedAccount: () => invoke<void>("adopt_linked_account"),
  /** "Stay signed in": resume a saved session from the OS keychain (null = none). */
  autoLogin: () => invoke<UserInfo | null>("auto_login"),
  /** Forget the saved keychain session so the next launch won't auto-login. */
  clearSavedSession: () => invoke<void>("clear_saved_session"),
};
