import { invoke } from "@tauri-apps/api/core";
import type { MessageProtection } from "@/lib/types";

export interface DraftSnapshot {
  session: string;
  drafts: Record<string, string>;
  protections?: Record<string, MessageProtection>;
}
export const draftsApi = {
  load: (owner: string) =>
    invoke<DraftSnapshot>("load_conversation_drafts", { owner }),
  save: (
    owner: string,
    session: string,
    drafts: Record<string, string>,
    protections: Record<string, MessageProtection>,
  ) =>
    invoke<void>("save_conversation_drafts", {
      owner,
      session,
      drafts,
      protections,
    }),
};
