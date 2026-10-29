import type { Page } from "@playwright/test";

/** Observe transient feedback before navigation, including virtualized insertion. */
export async function expectMessageHighlight(
  page: Page,
  selectedId: string,
  navigate: () => Promise<unknown>,
  otherId?: string,
) {
  // Latch visibility and the exact class in one browser observation. Separate
  // controller assertions can miss the unchanged 2.8s production highlight.
  const observation = page.waitForFunction(
    ({ selectedId, otherId }) => {
      const rows = [
        ...document.querySelectorAll<HTMLElement>("[data-message-id]"),
      ];
      const selected = rows.find((row) => row.dataset.messageId === selectedId);
      const other = rows.find((row) => row.dataset.messageId === otherId);
      if (!selected) return false;
      const bounds = selected.getBoundingClientRect();
      return (
        bounds.width > 0 &&
        bounds.height > 0 &&
        getComputedStyle(selected).visibility === "visible" &&
        selected.classList.contains("bg-signal/10") &&
        rows.filter((row) => row.classList.contains("bg-signal/10")).length ===
          1 &&
        !other?.classList.contains("bg-signal/10")
      );
    },
    { selectedId, otherId },
    { timeout: 5000 },
  );
  const [highlight] = await Promise.all([observation, navigate()]);
  await highlight.dispose();
}
