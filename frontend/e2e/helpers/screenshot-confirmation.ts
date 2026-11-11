/** Screenshot dismissal retains its existing five-second observation budget. */
export function waitForScreenshotEditorDismissal(): Promise<number> {
  return new Promise((resolve, reject) => {
    const started = performance.now();
    const deadline = started + 5000;
    const cleanup = () => {
      observer.disconnect();
      clearTimeout(timer);
    };
    const expired = () => {
      cleanup();
      reject(new Error("Screenshot editor dismissal exceeded 5000ms"));
    };
    const check = () => {
      if (performance.now() >= deadline) return expired();
      const dismissed = !document.querySelector(
        '[data-testid="screenshot-editor"]',
      );
      if (performance.now() >= deadline) return expired();
      if (dismissed) {
        cleanup();
        resolve(performance.now() - started);
      }
    };
    const observer = new MutationObserver(check);
    const timer = setTimeout(
      expired,
      Math.max(0, deadline - performance.now()),
    );
    observer.observe(document.body, { subtree: true, childList: true });
    check();
  });
}
