/** Import only on demand; failures remain observable and retryable by the dialog. */
export function loadHelpReader() {
  return import("./HelpReader");
}
