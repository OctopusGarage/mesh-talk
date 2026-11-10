import { invoke } from "@tauri-apps/api/core";
import type { PrivacySnapshot } from "../types";

export interface AccountBindingChange {
  device_id: string;
  previous_account_id: string;
  current_account_id: string;
}

export const privacy = {
  bindingChanges: (owner: string, account: string) =>
    invoke<{ owner: string; changes: AccountBindingChange[] }>(
      "get_account_binding_changes",
      { owner, account },
    ),
  acceptBindingChange: (owner: string, change: AccountBindingChange) =>
    invoke<PrivacySnapshot>("accept_account_binding_change", { owner, change }),
  initiateContact: (owner: string, account: string) =>
    invoke<PrivacySnapshot>("initiate_privacy_contact", { owner, account }),
  get: (owner: string) => invoke<PrivacySnapshot>("get_privacy", { owner }),
  setInvisible: (owner: string, invisible: boolean) =>
    invoke<PrivacySnapshot>("set_invisible", { owner, invisible }),
  setAllowed: (owner: string, account: string, allowed: boolean) =>
    invoke<PrivacySnapshot>("set_privacy_allowed", { owner, account, allowed }),
};
