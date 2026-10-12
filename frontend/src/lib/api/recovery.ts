import { invoke } from "@tauri-apps/api/core";

export interface BackupPreview {
  account_id: string;
  created_at_ms: number;
  messages: number;
  channels: number;
  file_cards: number;
  media_files: number;
  missing_media: number;
}

export interface BackupCreated {
  recovery_key: string;
  preview: BackupPreview;
  archive_bytes: number;
}

export interface BackupStatus {
  destination: string;
  preview: BackupPreview;
  archive_bytes: number;
}

export interface RestoreComplete {
  username: string;
  device_id: string;
  preview: BackupPreview;
}

export const recovery = {
  prepareKey: () => invoke<string>("prepare_backup_key"),
  status: () => invoke<BackupStatus | null>("backup_status"),
  create: (destination: string, recoveryKey: string) =>
    invoke<BackupCreated>("create_backup", { destination, recoveryKey }),
  inspect: (path: string, recoveryKey: string) =>
    invoke<BackupPreview>("inspect_backup", { path, recoveryKey }),
  restore: (
    path: string,
    recoveryKey: string,
    username: string,
    newPassword: string,
  ) =>
    invoke<RestoreComplete>("restore_backup", {
      path,
      recoveryKey,
      username,
      newPassword,
    }),
};
