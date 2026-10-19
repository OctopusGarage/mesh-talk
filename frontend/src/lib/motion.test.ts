// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useMotionOK } from "./motion";

let root: Root;
let container: HTMLDivElement;
let reduced: boolean;
let listeners: Set<() => void>;

function Probe() {
  return createElement("span", null, String(useMotionOK()));
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  reduced = false;
  listeners = new Set();
  vi.stubGlobal(
    "matchMedia",
    vi.fn(() => ({
      get matches() {
        return reduced;
      },
      addEventListener: (_event: string, listener: () => void) =>
        listeners.add(listener),
      removeEventListener: (_event: string, listener: () => void) =>
        listeners.delete(listener),
    })),
  );
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

it("observes the initial reduced-motion preference before allowing animation", async () => {
  reduced = true;
  await act(async () => root.render(createElement(Probe)));
  expect(container.textContent).toBe("false");
});

it("updates mounted consumers when the operating system preference changes", async () => {
  await act(async () => root.render(createElement(Probe)));
  expect(container.textContent).toBe("true");
  await act(async () => {
    reduced = true;
    listeners.forEach((notify) => notify());
  });
  expect(container.textContent).toBe("false");
  await act(async () => {
    reduced = false;
    listeners.forEach((notify) => notify());
  });
  expect(container.textContent).toBe("true");
});

it("releases preference listeners on unmount", async () => {
  await act(async () => root.render(createElement(Probe)));
  expect(listeners.size).toBeGreaterThan(0);
  await act(async () => root.render(null));
  expect(listeners.size).toBe(0);
});

it("disables animation when preference observation is unavailable", async () => {
  vi.stubGlobal("matchMedia", undefined);
  await act(async () => root.render(createElement(Probe)));
  expect(container.textContent).toBe("false");
});
