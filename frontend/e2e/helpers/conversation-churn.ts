/** Self-contained browser journey; passed directly to Page.evaluate. */
export async function runConversationChurn({
  bob,
  channel,
  carol,
}: {
  bob: string;
  channel: string;
  carol: string;
}) {
  const visible = (element: Element | null): element is HTMLElement => {
    if (!(element instanceof HTMLElement)) return false;
    const rect = element.getBoundingClientRect();
    const style = getComputedStyle(element);
    return (
      rect.width > 0 &&
      rect.height > 0 &&
      style.visibility === "visible" &&
      style.display !== "none"
    );
  };
  const byTestId = (id: string) =>
    document.querySelector(`[data-testid="${id}"]`);
  const loaded = (text: string) => {
    const log = document.querySelector('[role="log"]');
    return (
      visible(log) &&
      [...log.querySelectorAll("*")].some(
        (element) => element.textContent === text && visible(element),
      )
    );
  };
  const waitFor = (ready: () => boolean, description: string) =>
    new Promise<void>((resolve, reject) => {
      let frame = 0;
      const cleanup = () => {
        observer.disconnect();
        clearTimeout(timer);
        cancelAnimationFrame(frame);
      };
      const check = () => {
        try {
          if (ready()) {
            cleanup();
            resolve();
          } else {
            scheduleCheck();
          }
        } catch (error) {
          cleanup();
          reject(error);
        }
      };
      const scheduleCheck = () => {
        if (!frame)
          frame = requestAnimationFrame(() => {
            frame = 0;
            check();
          });
      };
      const observer = new MutationObserver(check);
      const timer = setTimeout(() => {
        cleanup();
        reject(
          new Error(`Conversation readiness exceeded 5000ms: ${description}`),
        );
      }, 5000);
      observer.observe(document.body, {
        subtree: true,
        childList: true,
        attributes: true,
        characterData: true,
      });
      check();
    });
  const journeyStarted = performance.now();
  let activations = 0;
  for (let round = 0; round < 24; round++) {
    for (const id of [bob, channel, carol]) {
      const activationStarted = performance.now();
      const row = byTestId(`conversation-row-${id}`);
      if (!(row instanceof HTMLButtonElement) || !visible(row) || row.disabled)
        throw new Error(
          `Conversation button unavailable: ${id}, round ${round + 2}`,
        );
      row.focus();
      row.click();
      activations++;
      await waitFor(
        () =>
          visible(byTestId(`conversation-row-${id}`)) &&
          byTestId(`conversation-row-${id}`)?.getAttribute("aria-current") ===
            "true" &&
          (id === bob
            ? visible(byTestId("conversation-header")) && loaded("history 299")
            : id === channel
              ? loaded("channel kickoff")
              : visible(byTestId("conversation-empty"))),
        `${id}, round ${round + 2}`,
      );
      // At most 72 fixed-scenario records in the retained browser trace. This
      // describes completed observations, not the cause of any hosted delay.
      console.debug("memory-churn", {
        round: round + 2,
        destination: id,
        elapsedMs: Math.round(performance.now() - activationStarted),
        journeyMs: Math.round(performance.now() - journeyStarted),
      });
    }
  }
  return { rounds: 24, activations };
}
