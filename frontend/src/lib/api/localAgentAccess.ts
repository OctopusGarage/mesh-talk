import { invoke } from "@tauri-apps/api/core";

export type AgentDestination = { kind: "account" | "channel"; id: string };
export interface LocalAgentAccess {
  enabled: boolean;
  running: boolean;
  destinations: AgentDestination[];
  available_destinations: { destination: AgentDestination; label: string }[];
  instructions_path: string;
  instructions: string;
  error: string | null;
}
export const localAgentAccess = {
  get: (owner: string) =>
    invoke<LocalAgentAccess>("get_local_agent_access", { owner }),
  set: (owner: string, enabled: boolean, destinations: AgentDestination[]) =>
    invoke<LocalAgentAccess>("set_local_agent_access", {
      owner,
      enabled,
      destinations,
    }),
};
