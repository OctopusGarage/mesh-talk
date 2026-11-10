import { invoke } from "@tauri-apps/api/core";
import { invokeSensitive } from "./sensitiveInvoke";
import { readWithDeadline } from "../readDeadline";
import type {
  LoginResult,
  SavedSessionStatus,
  LogoutResult,
  RegisterResult,
  UserInfo,
} from "../types";

export interface EnrollmentSourceInfo {
  id: string;
  device_id: string;
  account_id: string;
  nickname: string;
  addresses: string[];
}

export const auth = {
  beginDeviceEnrollment: () =>
    invoke<{ id: string }>("begin_device_enrollment"),
  probeDeviceEnrollment: (enrollmentId: string, targets: string[]) =>
    invoke<EnrollmentSourceInfo[]>("probe_device_enrollment", {
      enrollmentId,
      targets,
    }),
  completeDeviceEnrollment: (
    enrollmentId: string,
    sourceId: string,
    code: string,
    includeHistory: boolean,
  ) =>
    invoke<LoginResult>("complete_device_enrollment", {
      enrollmentId,
      sourceId,
      code,
      includeHistory,
    }),
  cancelDeviceEnrollment: (enrollmentId: string) =>
    invoke<void>("cancel_device_enrollment", { enrollmentId }),
  login: (username: string, password: string) =>
    invoke<LoginResult>("login", { username, password }),
  register: (username: string, password: string) =>
    invoke<RegisterResult>("register", { username, password }),
  retrySavedSession: (owner: string) =>
    invokeSensitive<SavedSessionStatus>("retry_saved_session", { owner }),
  /** Change the editable display name (nickname); returns the updated user. */
  renameAccount: (newDisplayName: string) =>
    invoke<UserInfo>("rename_account", { newDisplayName }),
  logout: () => invoke<LogoutResult>("logout"),
  adoptLinkedAccount: () => invoke<void>("adopt_linked_account"),
  /** "Stay signed in": resume a saved session from the OS keychain (null = none). */
  autoLogin: () => invoke<UserInfo | null>("auto_login"),
  /** Forget the saved keychain session so the next launch won't auto-login. */
  clearSavedSession: () => invoke<void>("clear_saved_session"),
  lock: () => invoke<void>("lock_app"),
  lockStatus: () =>
    readWithDeadline(() => invoke<UserInfo | null>("lock_status")),
  unlock: (password?: string) =>
    invoke<void>("unlock_app", { password: password ?? null }),
  noteActivity: () => invoke<void>("note_activity"),
  setCallActive: (active: boolean) =>
    invoke<void>("set_call_active", { active }),
};
