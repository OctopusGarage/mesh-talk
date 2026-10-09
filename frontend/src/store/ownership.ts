import { useAuth } from "./auth";

let runtimeSnapshot = () => ({ runEpoch: 0, identityEpoch: 0 });

type RuntimeSnapshot = ReturnType<typeof runtimeSnapshot>;

/** Capture the owner and runtime generation once, then validate both after awaits. */
export function captureChatOwner(
  read: () => RuntimeSnapshot,
  identitySensitive = true,
) {
  const auth = captureOwner();
  const snapshot = read();
  return {
    ...auth,
    run: snapshot.runEpoch,
    current: () => {
      const now = read();
      return (
        auth.owner !== null &&
        auth.current() &&
        now.runEpoch === snapshot.runEpoch &&
        (!identitySensitive || now.identityEpoch === snapshot.identityEpoch)
      );
    },
  };
}

/** Register a read-only snapshot without coupling auxiliary stores back to chat. */
export function registerRuntimeSnapshot(read: typeof runtimeSnapshot) {
  runtimeSnapshot = read;
}

export function captureRuntimeOwner() {
  return captureChatOwner(runtimeSnapshot);
}

/** UI cancellation only: an already-admitted legacy native command is not aborted. */
export function captureOwner() {
  const { user, generation } = useAuth.getState();
  const owner = user?.id ?? null;
  return {
    owner,
    generation,
    current: () => {
      const now = useAuth.getState();
      return now.user?.id === owner && now.generation === generation;
    },
  };
}
