import { useSettings } from "@/store/settings";

/** Only clear text that this app copied and that the user has not replaced. */
export async function copySensitiveText(
  value: string,
  confirmMessage: string,
): Promise<boolean> {
  const policy = useSettings.getState().privacy;
  if (policy.confirm_sensitive_copy && !window.confirm(confirmMessage)) {
    return false;
  }
  await navigator.clipboard.writeText(value);
  if (policy.clipboard_clear_seconds > 0) {
    window.setTimeout(() => {
      void clearIfUnchanged(value);
    }, policy.clipboard_clear_seconds * 1000);
  }
  return true;
}

async function clearIfUnchanged(value: string): Promise<void> {
  try {
    if ((await navigator.clipboard.readText()) === value) {
      await navigator.clipboard.writeText("");
    }
  } catch {
    // Clipboard read permission is platform dependent; never overwrite an unknown value.
  }
}
