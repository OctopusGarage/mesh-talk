import { invoke } from "@tauri-apps/api/core";
import type { FavoriteMessage } from "./messageFavorites";
export interface ForwardTarget {
  kind: "account" | "channel";
  target: string;
}
export interface ForwardSource extends ForwardTarget {
  message_ids: string[];
}
export interface ForwardRequest {
  operation_id: string;
  source: ForwardSource;
  destinations: ForwardTarget[];
  mode: "individual" | "merged";
}
export interface ForwardResult {
  operation_id: string;
  items: Array<{
    target: ForwardTarget;
    source_id: string | null;
    message_id: string | null;
    status: "accepted" | "failed" | "pending";
    error: string | null;
  }>;
}
export interface ForwardBatch {
  request: ForwardRequest;
  result: ForwardResult;
}
export const messageForwarding = {
  exportAttachment: (
    owner: string,
    fileConv: string,
    index: number,
    destination: string,
  ) =>
    invoke<string>("owner_export_forwarded_attachment", {
      owner,
      fileConv,
      index,
      destination,
    }),
  inspect: (owner: string, source: ForwardSource) =>
    invoke<{ messages: FavoriteMessage[]; source_label: string }>(
      "owner_inspect_forward_messages",
      { owner, source },
    ),
  send: (owner: string, request: ForwardRequest) =>
    invoke<ForwardResult>("owner_forward_messages", { owner, request }),
  list: (owner: string) =>
    invoke<ForwardBatch[]>("owner_list_forward_batches", { owner }),
};
