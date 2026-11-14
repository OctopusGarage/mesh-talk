import { invoke } from "@tauri-apps/api/core";
import type {
  NodeStateChangedEvent,
  CallSignalEvent,
  ChannelMessageEvent,
  DmReceivedEvent,
  FileProgressEvent,
  FileReceivedEvent,
  ProfileReceivedEvent,
  ReceptionCursor,
  ReceptionPage,
  ReceptionScope,
} from "../types";

export type HostReception =
  | { kind: "dm"; event: DmReceivedEvent }
  | { kind: "channel"; event: ChannelMessageEvent }
  | { kind: "file"; event: FileReceivedEvent }
  | { kind: "profile"; event: ProfileReceivedEvent }
  | { kind: "call"; event: CallSignalEvent }
  | { kind: "progress"; event: FileProgressEvent }
  | {
      kind: "state";
      event?: { kinds: ReceptionKind[] };
    };

export type ReceptionKind = NodeStateChangedEvent["kind"];
const RECEPTION_KINDS: readonly string[] = [
  "state",
  "roster",
  "delivery",
  "files",
  "documents",
  "sync_status",
];

/** Older hosts omit metadata; malformed or future domains need conservative repair. */
export function receptionKinds(event: unknown): ReceptionKind[] {
  const kinds =
    event && typeof event === "object" && "kinds" in event
      ? event.kinds
      : undefined;
  if (
    !Array.isArray(kinds) ||
    kinds.length > RECEPTION_KINDS.length ||
    !kinds.every(
      (kind) => typeof kind === "string" && RECEPTION_KINDS.includes(kind),
    )
  )
    return ["state"];
  if (kinds.includes("state")) return ["state"];
  return [...new Set(kinds)] as ReceptionKind[];
}

export const reception = {
  pull: (
    scope: ReceptionScope,
    after: ReceptionCursor | null,
    through: ReceptionCursor | null,
    reset: boolean,
  ) =>
    invoke<ReceptionPage<HostReception>>("owner_reception_page", {
      scope,
      after,
      through,
      reset,
    }),
  acknowledge: (scope: ReceptionScope, page: ReceptionPage<HostReception>) =>
    invoke<boolean>("owner_reception_ack", {
      scope,
      token: page.token,
      after: page.after,
      next: page.next,
    }),
};
