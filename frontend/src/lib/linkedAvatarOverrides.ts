import { accountDocuments } from "./api/accountDocuments";
import { avatars } from "@/lib/api/contacts";
import { useAvatars } from "@/store/avatars";
import { useChat } from "@/store/chat";
import { captureRuntimeOwner } from "@/store/ownership";
import { subscribeSyncInvalidation } from "./syncInvalidation";
import { readWithDeadline } from "./readDeadline";

/** Only the explicit kept-source action attributes legacy overrides to an account. */
export async function publishAvatarOverridesBeforeLink(owner: string) {
  const lease = captureRuntimeOwner();
  const account = useChat.getState().myAccountId;
  if (!account || lease.owner !== owner || !lease.current())
    throw new Error("Account changed during avatar preparation");
  await avatars.publishLegacy(owner, account);
  if (!lease.current() || useChat.getState().myAccountId !== account)
    throw new Error("Account changed during avatar preparation");
  await useAvatars.getState().load(lease.current);
}

/** Network reconciliation is owned by the existing account worker; this only refreshes its UI projection. */
export function startLinkedAvatarOverrides(owner: string): () => void {
  const lease = captureRuntimeOwner();
  const account = useChat.getState().myAccountId;
  let active = true;
  let loading = false;
  let dirty = false;
  let digest: string | undefined;
  const current = () =>
    active &&
    lease.owner === owner &&
    lease.current() &&
    Boolean(account) &&
    useChat.getState().myAccountId === account;
  const refresh = async () => {
    if (!current()) return;
    if (loading) {
      dirty = true;
      return;
    }
    loading = true;
    try {
      const page = await readWithDeadline(() =>
        accountDocuments.list(owner, account!, "avatar_override", null, 1),
      );
      if (!current()) return;
      const next = JSON.stringify(page.digest);
      if (next !== digest) {
        const applied = await useAvatars.getState().load(current);
        if (applied && current()) digest = next;
      }
    } catch (error) {
      if (import.meta.env.PROD)
        console.warn("Could not refresh linked avatars", error);
    } finally {
      loading = false;
      if (dirty && current()) {
        dirty = false;
        queueMicrotask(() => void refresh());
      }
    }
  };
  const unlisten = subscribeSyncInvalidation((event) => {
    if (event.owner === owner && event.kind === "state") void refresh();
  });
  const timer = window.setInterval(() => void refresh(), 10_000);
  void refresh();
  return () => {
    active = false;
    unlisten();
    window.clearInterval(timer);
  };
}
