// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
const work = vi.hoisted(() => ({
  load: vi.fn(() => import("./HelpReader")),
  search: vi.fn(() => []),
  render: vi.fn(() => ""),
}));
vi.mock("../../../../tools/help/search.mjs", () => ({
  searchHelp: work.search,
}));
vi.mock("../../../../tools/help/render.mjs", () => ({
  renderMarkdown: work.render,
}));
vi.mock("./helpReaderLoader", () => ({ loadHelpReader: work.load }));
vi.mock("./content", () => ({
  HELP_VERSION: "test",
  HELP_LANGUAGE: "en",
  HELP_URL: "https://example.com/",
  helpTopics: [{ id: "first-run", title: "First run", body: "hello" }],
}));
vi.mock("./metadata", () => ({ HELP_VERSION: "test" }));
import { HelpDialog } from "./HelpDialog";
(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
beforeEach(() => {
  vi.clearAllMocks();
  HTMLElement.prototype.scrollTo = vi.fn();
});
afterEach(() => vi.restoreAllMocks());
it("does no reader search or Markdown preparation before help is opened", async () => {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  await act(async () =>
    root.render(
      createElement(HelpDialog, { open: false, onOpenChange: vi.fn() }),
    ),
  );
  try {
    expect(work.load).not.toHaveBeenCalled();
    expect(work.search).not.toHaveBeenCalled();
    expect(work.render).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
    host.remove();
  }
});

function mount() {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const render = (open: boolean) =>
    act(async () =>
      root.render(createElement(HelpDialog, { open, onOpenChange: vi.fn() })),
    );
  const close = async () => {
    await act(async () => root.unmount());
    host.remove();
  };
  return { render, close };
}
it("ignores a reader completion after help closes, then opens and focuses on demand", async () => {
  let resolve!: (module: typeof import("./HelpReader")) => void;
  work.load.mockImplementationOnce(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  const ui = mount();
  try {
    await ui.render(true);
    expect(work.load).toHaveBeenCalledOnce();
    await ui.render(false);
    const module = await import("./HelpReader");
    await act(async () => resolve(module));
    expect(work.render).not.toHaveBeenCalled();
    expect(document.querySelector('[data-testid="help-search"]')).toBeNull();
    await ui.render(true);
    expect(document.activeElement).toBe(
      document.querySelector('[data-testid="help-search"]'),
    );
    expect(work.render).toHaveBeenCalled();
  } finally {
    await ui.close();
  }
});
it("keeps import failure local and retries without replacing the dialog surface", async () => {
  work.load.mockRejectedValueOnce(new Error("offline asset unavailable"));
  const ui = mount();
  try {
    await ui.render(true);
    const dialog = document.querySelector('[data-testid="help-dialog"]');
    const retry = document.querySelector<HTMLButtonElement>(
      '[data-testid="help-retry"]',
    )!;
    expect(retry).not.toBeNull();
    await act(async () => retry.click());
    expect(document.querySelector('[data-testid="help-dialog"]')).toBe(dialog);
    expect(document.activeElement).toBe(
      document.querySelector('[data-testid="help-search"]'),
    );
    expect(work.load).toHaveBeenCalledTimes(2);
  } finally {
    await ui.close();
  }
});
it("ignores completion after the owning presentation unmounts", async () => {
  let resolve!: (module: typeof import("./HelpReader")) => void;
  work.load.mockImplementationOnce(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  const ui = mount();
  await ui.render(true);
  await ui.close();
  await act(async () => resolve(await import("./HelpReader")));
  expect(work.render).not.toHaveBeenCalled();
  expect(document.querySelector('[data-testid="help-dialog"]')).toBeNull();
});
