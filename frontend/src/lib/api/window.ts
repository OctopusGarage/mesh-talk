import { invoke } from "@tauri-apps/api/core";

/** One-shot native launch presentation; ordinary visibility remains user controlled. */
export const initialPresentationReady = () =>
  invoke<void>("initial_presentation_ready");
