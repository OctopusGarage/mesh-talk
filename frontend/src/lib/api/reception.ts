import { invoke } from "@tauri-apps/api/core";
import type {
  ReceptionCursor,
  ReceptionPage,
  ReceptionScope,
} from "@/store/receptionDelivery";
import type {
  CallSignalEvent,
  ChannelMessageEvent,
  DmReceivedEvent,
  FileProgressEvent,
  FileReceivedEvent,
  ProfileReceivedEvent,
} from "../types";

export type HostReception =
  | { kind: "dm"; event: DmReceivedEvent }
  | { kind: "channel"; event: ChannelMessageEvent }
  | { kind: "file"; event: FileReceivedEvent }
  | { kind: "profile"; event: ProfileReceivedEvent }
  | { kind: "call"; event: CallSignalEvent }
  | { kind: "progress"; event: FileProgressEvent }
  | { kind: "state" };

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
