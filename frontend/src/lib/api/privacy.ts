import { invoke } from "@tauri-apps/api/core";
import type { PrivacySnapshot } from "../types";

export const privacy = {
  initiateContact: (owner: string, account: string) =>
    invoke<PrivacySnapshot>("initiate_privacy_contact", { owner, account }),
  get: (owner: string) => invoke<PrivacySnapshot>("get_privacy", { owner }),
  setInvisible: (owner: string, invisible: boolean) =>
    invoke<PrivacySnapshot>("set_invisible", { owner, invisible }),
  setAllowed: (owner: string, account: string, allowed: boolean) =>
    invoke<PrivacySnapshot>("set_privacy_allowed", { owner, account, allowed }),
};
