import { invoke } from "@tauri-apps/api/core";

export interface DraftSnapshot {
  session: string;
  drafts: Record<string, string>;
}
export const draftsApi = {
  load: (owner: string) =>
    invoke<DraftSnapshot>("load_conversation_drafts", { owner }),
  save: (owner: string, session: string, drafts: Record<string, string>) =>
    invoke<void>("save_conversation_drafts", { owner, session, drafts }),
};
