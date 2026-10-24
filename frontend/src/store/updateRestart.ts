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

/** Persist a stable draft snapshot, then guard the synchronous installer handoff. */
export async function prepareUpdateRestart(): Promise<() => void> {
  assertNoActiveWork();
  const snapshot = useDrafts.getState();
  const assertReady = () => {
    const current = useDrafts.getState();
    if (
      current.error ||
      current.owner !== snapshot.owner ||
      current.generation !== snapshot.generation ||
      current.loaded !== snapshot.loaded ||
      current.texts !== snapshot.texts ||
      current.protections !== snapshot.protections
    )
      throw new Error(i18n.t("about.updates.saveDraftsFailed"));
    assertNoActiveWork();
  };
  await snapshot.flush();
  // A concurrent edit stays in the draft store and defers restart. The next
  // attempt saves that revision; never install from a stale durable snapshot.
  assertReady();
  return assertReady;
}
