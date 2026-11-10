import { invoke } from "@tauri-apps/api/core";
import { invokeSensitive } from "./sensitiveInvoke";
import type { BackupCreated, BackupPreview, RestoreComplete } from "./recovery";
export interface DriveArchive {
  id: string;
  name: string;
  size: string;
  checksum: string;
}
export interface DriveProgress {
  phase: string;
  transferred: number;
  total: number;
}
export const googleDrive = {
  available: () => invoke<boolean>("google_drive_available"),
  connect: (connectionId: string) =>
    invoke<void>("google_drive_connect", { connectionId }),
  resume: (connectionId: string) =>
    invoke<boolean>("google_drive_resume", { connectionId }),
  forget: (connectionId: string) =>
    invoke<void>("google_drive_forget", { connectionId }),
  disconnect: (connectionId: string) =>
    invoke<void>("google_drive_disconnect", { connectionId }),
  progress: (connectionId: string) =>
    invoke<DriveProgress>("google_drive_progress", { connectionId }),
  list: (connectionId: string) =>
    invoke<DriveArchive[]>("google_drive_list", { connectionId }),
  create: (connectionId: string, recoveryKey?: string) =>
    invokeSensitive<BackupCreated>("google_drive_create_backup", {
      connectionId,
      recoveryKey,
    }),
  download: (connectionId: string, fileId: string) =>
    invoke<void>("google_drive_download", { connectionId, fileId }),
  inspect: (connectionId: string, recoveryKey: string) =>
    invoke<BackupPreview>("google_drive_inspect", {
      connectionId,
      recoveryKey,
    }),
  restore: (
    connectionId: string,
    recoveryKey: string,
    username: string,
    newPassword: string,
  ) =>
    invoke<RestoreComplete>("google_drive_restore", {
      connectionId,
      recoveryKey,
      username,
      newPassword,
    }),
};
