import { useAuth } from "./auth";

let runtimeSnapshot = () => ({ runEpoch: 0, identityEpoch: 0 });

/** Register a read-only snapshot without coupling auxiliary stores back to chat. */
export function registerRuntimeSnapshot(read: typeof runtimeSnapshot) {
  runtimeSnapshot = read;
}

export function captureRuntimeOwner() {
  const owner = captureOwner();
  const snapshot = runtimeSnapshot();
  return {
    ...owner,
    current: () => {
      const now = runtimeSnapshot();
      return (
        owner.current() &&
        now.runEpoch === snapshot.runEpoch &&
        now.identityEpoch === snapshot.identityEpoch
      );
    },
  };
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
