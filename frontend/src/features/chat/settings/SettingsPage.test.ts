// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import type { SettingsController } from "./useSettingsController";
const { load } = vi.hoisted(() => ({ load: vi.fn() }));
vi.mock("./loadSettingsPage", () => ({ loadSettingsPage: load }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
import { SettingsPage } from "./SettingsPage";
(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
afterEach(() => vi.resetAllMocks());
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}
function fixture() {
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  const onReady = vi.fn();
  const props = {
    category: "general" as const,
    enabled: true,
    controller: { identity: "owner:1", settings: {} } as SettingsController,
    onReady,
    onDraftChange: vi.fn(),
    onAgentSettings: vi.fn(),
    onNotifications: vi.fn(),
  };
  return {
    host,
    props,
    onReady,
    render: async (extra: Partial<typeof props> = {}) => {
      await act(async () =>
        root.render(createElement(SettingsPage, { ...props, ...extra })),
      );
    },
    close: async () => {
      await act(async () => root.unmount());
      host.remove();
    },
  };
}
it("does not load a closed page or mount its late result", async () => {
  const pending = deferred<() => ReturnType<typeof createElement>>();
  load.mockReturnValue(pending.promise);
  const f = fixture();
  const mounted = vi.fn();
  try {
    await f.render({ enabled: false });
    expect(load).not.toHaveBeenCalled();
    await f.render();
    expect(load).toHaveBeenCalledOnce();
    await f.render({ enabled: false });
    await act(async () =>
      pending.resolve(() => {
        mounted();
        return createElement("div", null, "late");
      }),
    );
    expect(mounted).not.toHaveBeenCalled();
    expect(f.onReady).not.toHaveBeenCalled();
  } finally {
    await f.close();
  }
});
it("keeps a replaced owner's late module out of the new view", async () => {
  const old = deferred<() => ReturnType<typeof createElement>>();
  load
    .mockReturnValueOnce(old.promise)
    .mockResolvedValueOnce(() => createElement("div", null, "new owner"));
  const f = fixture();
  try {
    await f.render();
    await f.render({
      controller: { ...f.props.controller, identity: "owner:2" },
    });
    expect(f.host.textContent).toBe("new owner");
    await act(async () =>
      old.resolve(() => createElement("div", null, "old owner")),
    );
    expect(f.host.textContent).toBe("new owner");
  } finally {
    await f.close();
  }
});
it("retries a failed load locally without retiring the surrounding settings view", async () => {
  load
    .mockRejectedValueOnce(new Error("chunk unavailable"))
    .mockResolvedValueOnce(() => createElement("div", null, "ready"));
  const f = fixture();
  try {
    await f.render();
    expect(f.host.querySelector('[role="alert"]')).not.toBeNull();
    await act(async () =>
      (f.host.querySelector("button") as HTMLButtonElement).click(),
    );
    expect(load).toHaveBeenCalledTimes(2);
    expect(f.host.textContent).toBe("ready");
    expect(f.onReady).toHaveBeenCalledOnce();
  } finally {
    await f.close();
  }
});
