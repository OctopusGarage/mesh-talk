export type ConnectionState =
  "failed" | "starting" | "no-network" | "searching" | "ready";

/** One presentation model for the shell and empty conversation view. */
export function connectionState({
  bootFailed,
  ready,
  noNetwork,
  onlinePeople,
}: {
  bootFailed: boolean;
  ready: boolean;
  noNetwork: boolean;
  onlinePeople: number;
}): ConnectionState {
  if (bootFailed) return "failed";
  if (!ready) return "starting";
  if (noNetwork) return "no-network";
  if (onlinePeople === 0) return "searching";
  return "ready";
}

export const CONNECTION_LABEL: Record<ConnectionState, string> = {
  failed: "sidebar.nodeUnavailable",
  starting: "redesign.connectionStarting",
  "no-network": "redesign.noNetworkStatus",
  searching: "redesign.searchingStatus",
  ready: "redesign.connectionReady",
};
