import i18n from "@/lib/i18n";
import { useCalls } from "./calls";
import { useDrafts } from "./drafts";
import { useTransfers } from "./transfers";

function assertNoActiveWork() {
  const phase = useCalls.getState().phase;
  if (phase !== "idle" && phase !== "ended")
    throw new Error(i18n.t("about.updates.finishCall"));
  if (
    Object.values(useTransfers.getState().transfers).some(
      (transfer) => !transfer.delivered,
    )
  )
    throw new Error(i18n.t("about.updates.finishTransfers"));
}

/** Persist pending text before the host hands control to the native installer. */
export async function prepareUpdateRestart(): Promise<void> {
  assertNoActiveWork();
  await useDrafts.getState().flush();
  if (useDrafts.getState().error)
    throw new Error(i18n.t("about.updates.saveDraftsFailed"));
  assertNoActiveWork();
}
