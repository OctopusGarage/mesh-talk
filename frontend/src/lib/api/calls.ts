import { invoke } from "@tauri-apps/api/core";

/** Real-time call signaling: send an opaque WebRTC payload (SDP / "bye") to a specific
 * device (peer user_id). Device-addressed + ephemeral; inbound signals arrive as the
 * `call-signal` Tauri event (see store/calls.ts). */
export const calls = {
  signal: (target: string, payload: string) =>
    invoke<void>("send_call_signal", { target, payload }),
};
