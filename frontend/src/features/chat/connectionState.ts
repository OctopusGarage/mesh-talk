export type ConnectionState =
  "failed" | "starting" | "no-network" | "reconnecting" | "searching" | "ready";

/** One presentation model for the shell and empty conversation view. */
export function connectionState({
  bootFailed,
  ready,
  noNetwork,
  onlinePeople,
  reconnecting = false,
}: {
  bootFailed: boolean;
  ready: boolean;
  noNetwork: boolean;
  onlinePeople: number;
  reconnecting?: boolean;
}): ConnectionState {
  if (bootFailed) return "failed";
  if (!ready) return "starting";
  if (noNetwork) return "no-network";
  if (onlinePeople === 0 && reconnecting) return "reconnecting";
  if (onlinePeople === 0) return "searching";
  return "ready";
}

export const CONNECTION_LABEL: Record<ConnectionState, string> = {
  failed: "sidebar.nodeUnavailable",
  starting: "redesign.connectionStarting",
  "no-network": "redesign.noNetworkStatus",
  reconnecting: "presence.reconnecting",
  searching: "redesign.searchingStatus",
  ready: "redesign.connectionReady",
};
