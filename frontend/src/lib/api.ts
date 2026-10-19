// Stable public IPC facade. Feature modules own command names and payloads.
// Tauri v2 maps camelCase JS arg keys to the Rust commands' snake_case params.
export { privacy } from "./api/privacy";
export { auth } from "./api/auth";
export { chat } from "./api/chat";
export { calls } from "./api/calls";
export { diag, presence, obs, updates } from "./api/diagnostics";
export { contactPolicy, favorites, avatars } from "./api/contacts";
import { useAuth } from "@/store/auth";
import { createSettingsApi } from "./api/settings";

// App composition: low-level IPC modules do not read Zustand state.
export const settings = createSettingsApi(
  () => useAuth.getState().user?.id ?? null,
);
export { recovery } from "./api/recovery";
