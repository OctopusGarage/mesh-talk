import { invoke } from "@tauri-apps/api/core";
export interface DmLanguagePolicy {
  language: string | null;
  min_target_percent: number;
  supported_languages: string[];
}
export interface LanguageVerdict {
  reason?: string;
  allowed: boolean;
  target_percent: number;
  language: string;
}
export const dmLanguage = {
  get: (owner: string, account?: string) =>
    invoke<DmLanguagePolicy>("dm_language_policy", {
      owner,
      account: account ?? null,
    }),
  set: (owner: string, language: string | null) =>
    invoke<DmLanguagePolicy>("set_dm_language_policy", { owner, language }),
  validate: (owner: string, account: string, text: string) =>
    invoke<LanguageVerdict>("validate_dm_language", { owner, account, text }),
};
