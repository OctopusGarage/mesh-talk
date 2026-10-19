// @vitest-environment jsdom
import { act, createElement, type ChangeEvent } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

beforeEach(() => vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true));
afterEach(() => vi.unstubAllGlobals());

const mocks = vi.hoisted(() => ({
  probe: vi.fn(),
  rescan: vi.fn(),
}));
let changeEndpoint:
  ((event: ChangeEvent<HTMLInputElement>) => void) | undefined;
vi.mock("@/lib/api", () => ({
  diag: { probeEndpoint: mocks.probe, rescan: mocks.rescan },
}));
vi.mock("@/lib/motion", () => ({ useMotionOK: () => false }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@/components/ui/dialog", async () => {
  const { createElement } = await import("react");
  const wrap = ({ children }: { children: React.ReactNode }) =>
    createElement("div", null, children);
  return {
    Dialog: ({
      open,
      children,
    }: {
      open: boolean;
      children: React.ReactNode;
    }) => (open ? createElement("div", null, children) : null),
    DialogContent: wrap,
    DialogHeader: wrap,
    DialogTitle: wrap,
    DialogDescription: wrap,
  };
});
vi.mock("@/components/ui/button", async () => {
  const { createElement } = await import("react");
  return {
    Button: ({
      children,
      variant: _variant,
      size: _size,
      ...props
    }: React.ButtonHTMLAttributes<HTMLButtonElement> & {
      variant?: string;
      size?: string;
    }) => createElement("button", props, children),
  };
});
vi.mock("@/components/ui/input", async () => {
  const { createElement } = await import("react");
  return {
    Input: ({
      onChange,
      ...props
    }: React.InputHTMLAttributes<HTMLInputElement>) => {
      changeEndpoint = onChange;
      return createElement("input", { onChange, ...props });
    },
  };
});

import { OfflineConnectDialog } from "./OfflineConnectDialog";

it("clears a pending probe when its controlled dialog closes externally", async () => {
  let finish!: () => void;
  mocks.probe.mockReturnValue(
    new Promise<void>((resolve) => {
      finish = resolve;
    }),
  );
  const container = document.createElement("div");
  const root = createRoot(container);
  const render = async (open: boolean) => {
    await act(async () =>
      root.render(
        createElement(OfflineConnectDialog, {
          open,
          onOpenChange: () => {},
        }),
      ),
    );
  };
  try {
    await render(true);
    await act(async () =>
      changeEndpoint?.({
        target: { value: "192.0.2.1:47474" },
      } as ChangeEvent<HTMLInputElement>),
    );
    await act(async () =>
      container
        .querySelector("form")
        ?.dispatchEvent(
          new Event("submit", { bubbles: true, cancelable: true }),
        ),
    );
    expect(mocks.probe).toHaveBeenCalledWith("192.0.2.1:47474");
    await render(false);
    await act(async () => finish());
    await render(true);
    expect(container.textContent).not.toContain("offlineConnect.manualSent");
  } finally {
    await act(async () => root.unmount());
  }
});
