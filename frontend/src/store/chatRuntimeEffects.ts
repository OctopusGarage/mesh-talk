import type {
  CallSignalEvent,
  FileProgressEvent,
  ProfileReceivedEvent,
} from "@/lib/types";
import { useAvatars } from "./avatars";
import { useCalls } from "./calls";
import { resetFileAvailability } from "./fileAvailability";
import { useTransfers } from "./transfers";

/** Side effects owned by other stores during a chat runtime's lifecycle. */
export function resetChatFiles() {
  useTransfers.getState().reset();
  resetFileAvailability();
}

export function stopChatCalls() {
  useCalls.getState().teardown();
}

export function syncChatAvatarIdentity(
  accountId: string,
  isCurrent: () => boolean,
) {
  // Pull durable peer avatars and republish our own after a node restart.
  useAvatars.getState().setOwnId(accountId);
  void useAvatars.getState().loadPeers(isCurrent);
  void useAvatars.getState().reassertOwn(isCurrent);
}

export function applyChatFileProgress(event: FileProgressEvent) {
  useTransfers.getState().applyProgress(event);
}

export function applyChatProfile(event: ProfileReceivedEvent) {
  useAvatars.getState().mergeReceived(event.account_id, event.avatar);
}

export function applyChatCallSignal(event: CallSignalEvent) {
  useCalls.getState().onSignal(event);
}

export { resetFileAvailability as resetChatFileAvailability };
