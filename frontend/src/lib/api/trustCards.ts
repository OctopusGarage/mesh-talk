import { invoke } from "@tauri-apps/api/core";
export interface TrustCardView {
  account_id: string;
  revision: number;
  display_name: string;
  bio: string;
  created_at_ms: number | null;
  avatar: string | null;
  introductions: Array<{
    issuer_account_id: string;
    issuer_verified?: boolean;
    issued_at_ms: number;
    note: string;
  }>;
  witnesses: Array<{
    issuer_account_id: string;
    issuer_verified?: boolean;
    observed_at_ms: number;
  }>;
  signed_json: string;
}
export const trustCards = {
  get: (owner: string, accountId: string) =>
    invoke<TrustCardView | null>("get_trust_card", { owner, accountId }),
  publish: (
    owner: string,
    displayName: string,
    bio: string,
    createdAtMs: number | null,
  ) =>
    invoke<void>("publish_trust_card", {
      owner,
      displayName,
      bio,
      createdAtMs,
    }),
  importArtifact: (owner: string, json: string) =>
    invoke<void>("import_trust_artifact", { owner, json }),
  introduce: (owner: string, accountId: string, note: string) =>
    invoke<string>("create_trust_introduction", { owner, accountId, note }),
  witness: (owner: string, accountId: string) =>
    invoke<string>("witness_trust_creation", { owner, accountId }),
};
