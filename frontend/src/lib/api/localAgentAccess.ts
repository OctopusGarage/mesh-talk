import { invoke } from "@tauri-apps/api/core";
import { invokeSensitive } from "./sensitiveInvoke";

export type AgentDestination = { kind: "account" | "channel"; id: string };
export type AgentAccessLifetime = "one_hour" | "persistent";
export interface AgentAccessChange {
  lifetime?: AgentAccessLifetime;
  renew?: boolean;
}
export interface LocalAgentAccess {
  lifetime: AgentAccessLifetime;
  expires_at_ms: number | null;
  expired: boolean;
  recent_activity: {
    at_ms: number;
    destination: AgentDestination;
    action: "accepted" | "retried";
  }[];
  activity_error: string | null;
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
  set: (
    owner: string,
    enabled: boolean,
    destinations: AgentDestination[],
    change?: AgentAccessChange,
  ) =>
    invokeSensitive<LocalAgentAccess>("set_local_agent_access", {
      owner,
      enabled,
      destinations,
      ...change,
    }),
};
