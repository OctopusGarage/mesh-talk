import { invoke } from "@tauri-apps/api/core";
import { invokeSensitive } from "./sensitiveInvoke";
export interface TotpStatus {
  enabled: boolean;
  generation: number;
  recovery_remaining: number;
}
export interface TotpSetup {
  manual_seed: string;
  otpauth_uri: string;
  recovery_codes: string[];
}
export interface TotpMutation {
  sync_pending: boolean;
}
export const accountTotp = {
  status: (owner: string) =>
    invoke<TotpStatus>("account_totp_status", { owner }),
  begin: (owner: string) =>
    invokeSensitive<TotpSetup>("account_totp_begin", { owner }),
  confirm: (owner: string, code: string) =>
    invokeSensitive<TotpMutation>("account_totp_confirm", { owner, code }),
  disable: (owner: string, code: string) =>
    invokeSensitive<TotpMutation>("account_totp_disable", { owner, code }),
  cancel: (owner: string) =>
    invokeSensitive<void>("account_totp_cancel", { owner }),
  retry: (owner: string) =>
    invokeSensitive<TotpMutation>("account_totp_retry_sync", { owner }),
};
