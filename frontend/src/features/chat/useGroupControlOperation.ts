import { useEffect, useRef, useState } from "react";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import {
  unresolvedGroupLeave,
  type GroupOperationOutcome,
} from "@/store/accountGroupOperations";

/** Dialog state only: the existing native-backed ticket owns durable operation state. */
export function useGroupControlOperation(group: string | null) {
  const owner = useAuth((s) => s.user?.id);
  const generation = useAuth((s) => s.generation);
  const account = useChat((s) => s.myAccountId);
  const run = useChat((s) => s.runEpoch);
  const identity = useChat((s) => s.identityEpoch);
  const scope = JSON.stringify([
    owner,
    generation,
    account,
    run,
    identity,
    group,
  ]);
  const [local, setLocal] = useState<{
    scope: string;
    outcome?: GroupOperationOutcome;
    invoking: boolean;
  } | null>(null);
  const lifetime = useRef({ scope, live: true });
  lifetime.current.scope = scope;
  useEffect(() => {
    const currentLifetime = lifetime.current;
    currentLifetime.live = true;
    return () => {
      currentLifetime.live = false;
    };
  }, []);
  const current = local?.scope === scope ? local : null;
  const localId =
    current?.outcome && "id" in current.outcome
      ? current.outcome.id
      : undefined;
  const ticket = useChat((s) =>
    localId
      ? s.groupOperations?.[localId]
      : owner && account && group
        ? unresolvedGroupLeave(
            Object.values(s.groupOperations ?? {}),
            account,
            group,
          )
        : undefined,
  );
  const id = localId ?? ticket?.id;
  const status =
    ticket && ticket.account_id === account && ticket.group === group
      ? ticket.status
      : undefined;
  const outcome: GroupOperationOutcome | undefined =
    status && id
      ? {
          kind:
            status.phase.kind === "committed"
              ? "committed"
              : status.phase.kind === "cancelled"
                ? "cancelled"
                : status.phase.kind === "rejected"
                  ? "rejected"
                  : "pending",
          id,
          status,
        }
      : (current?.outcome ?? (id ? { kind: "unconfirmed", id } : undefined));
  const invoking = useRef<string | null>(null);
  const completion = useRef<{
    scope: string;
    id?: string;
    done(): void;
  } | null>(null);
  useEffect(() => {
    if (outcome?.kind === "committed" && completion.current?.scope === scope) {
      const done = completion.current.done;
      completion.current = null;
      done();
    }
  }, [outcome?.kind, scope]);
  const busy =
    current?.invoking === true ||
    outcome?.kind === "pending" ||
    outcome?.kind === "unconfirmed";
  async function runOperation(
    invoke: () => Promise<GroupOperationOutcome>,
    done: () => void = () => {},
  ) {
    if (busy || invoking.current === scope) return;
    invoking.current = scope;
    const captured = scope;
    const stillCurrent = () =>
      lifetime.current.live && lifetime.current.scope === captured;
    setLocal({ scope, invoking: true });
    completion.current = { scope, done };
    try {
      const result = await invoke();
      if (!stillCurrent()) return;
      setLocal({ scope, invoking: false, outcome: result });
      if (result.kind === "committed") {
        completion.current = null;
        done();
      }
    } catch (error) {
      if (stillCurrent())
        setLocal({
          scope,
          invoking: false,
          outcome: { kind: "not_admitted", error },
        });
    } finally {
      if (invoking.current === captured) invoking.current = null;
    }
  }
  return {
    busy,
    outcome,
    run: runOperation,
    id,
    cancelRequested:
      ticket?.cancelRequested === true ||
      status?.phase.kind === "cancellation_requested",
  };
}
