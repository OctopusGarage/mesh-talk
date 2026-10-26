import { invoke } from "@tauri-apps/api/core";
export interface AccountProfileView {
  account_id: string;
  display_name: string;
  bio: string;
  created_at_ms: number | null;
}
export const accountProfile = {
  get: (owner: string, accountId: string) =>
    invoke<AccountProfileView | null>("get_account_profile", {
      owner,
      accountId,
    }),
  saveBio: (owner: string, bio: string) =>
    invoke<void>("save_account_bio", { owner, bio }),
};
