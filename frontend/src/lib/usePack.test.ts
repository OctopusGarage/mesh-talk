// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { create } from "zustand";
import type { CustomizationPack } from "./pack";
const fixture = vi.hoisted(() => ({ read: vi.fn() }));
vi.mock("@/store/packs", () => ({
  usePacks: create(() => ({
    scope: {},
    packs: [{ id: "one", kind: "avatar" }],
    readPack: fixture.read,
  })),
}));
import { useThemePreview, usePackRequest, usePack } from "./usePack";
import { usePacks } from "@/store/packs";
const container = document.createElement("div");
const root = createRoot(container);
function Probe({ active }: { active: boolean }) {
  return createElement("span", null, usePack("one", active)?.name ?? "empty");
}
afterEach(() => {
  fixture.read.mockReset();
});
it("does not read closed galleries and discards a held read after close", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  let finish!: (pack: CustomizationPack) => void;
  fixture.read.mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  await act(async () => root.render(createElement(Probe, { active: false })));
  expect(fixture.read).not.toHaveBeenCalled();
  await act(async () => root.render(createElement(Probe, { active: true })));
  expect(fixture.read).toHaveBeenCalledExactlyOnceWith(
    "one",
    expect.any(Function),
  );
  await act(async () => root.render(createElement(Probe, { active: false })));
  await act(async () =>
    finish({ id: "one", name: "late" } as CustomizationPack),
  );
  expect(container.textContent).toBe("empty");
  // Reopening starts a fresh request. Replacing a catalog row fences that result
  // even when the installed library keeps its public identity.
  await act(async () => root.render(createElement(Probe, { active: true })));
  const oldFinish = finish;
  await act(async () =>
    usePacks.setState((state) => ({
      packs: state.packs.map((pack) => ({ ...pack })),
    })),
  );
  await act(async () =>
    oldFinish({ id: "one", name: "replaced" } as CustomizationPack),
  );
  expect(container.textContent).toBe("empty");
  const accountFinish = finish;
  await act(async () => usePacks.setState({ scope: null }));
  await act(async () =>
    accountFinish({ id: "one", name: "old account" } as CustomizationPack),
  );
  expect(container.textContent).toBe("empty");
  const deletedFinish = finish;
  await act(async () => usePacks.setState({ packs: [] }));
  await act(async () =>
    deletedFinish({ id: "one", name: "deleted" } as CustomizationPack),
  );
  expect(container.textContent).toBe("empty");
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});

it("exposes a failed demand read and retries without changing the library", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const retryRoot = createRoot(container);
  usePacks.setState({
    packs: [{ id: "one", kind: "avatar" } as CustomizationPack],
  });
  fixture.read.mockRejectedValueOnce(new Error("temporary read failure"));
  let request!: ReturnType<typeof usePackRequest>;
  function RequestProbe() {
    request = usePackRequest("one");
    return createElement(
      "span",
      null,
      request.error ? "failed" : (request.pack?.name ?? "loading"),
    );
  }
  await act(async () => retryRoot.render(createElement(RequestProbe)));
  expect(container.textContent).toBe("failed");
  fixture.read.mockResolvedValueOnce({ id: "one", name: "Recovered" });
  await act(async () => request.retry());
  expect(container.textContent).toBe("Recovered");
  expect(fixture.read).toHaveBeenCalledTimes(2);
  await act(async () => retryRoot.unmount());
  vi.unstubAllGlobals();
});

it("projects only a remembered theme preview while visible", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const previewRoot = createRoot(container);
  usePacks.setState({
    packs: [{ id: "one", kind: "theme" } as CustomizationPack],
  });
  fixture.read.mockResolvedValue({
    id: "one",
    kind: "theme",
    wallpapers: [
      { id: "first", url: "first-image" },
      { id: "remembered", url: "selected-image" },
    ],
    crest: "crest-image",
  });
  let preview!: ReturnType<typeof useThemePreview>;
  function PreviewProbe({ visible }: { visible: boolean }) {
    preview = useThemePreview("one", "remembered", visible);
    return createElement("span", null, preview.value ?? "swatch");
  }
  await act(async () =>
    previewRoot.render(createElement(PreviewProbe, { visible: false })),
  );
  expect(fixture.read).not.toHaveBeenCalled();
  await act(async () =>
    previewRoot.render(createElement(PreviewProbe, { visible: true })),
  );
  expect(preview.value).toBe("selected-image");
  expect(typeof preview.value).toBe("string");
  await act(async () =>
    previewRoot.render(createElement(PreviewProbe, { visible: false })),
  );
  expect(preview.value).toBeUndefined();
  await act(async () => previewRoot.unmount());
  vi.unstubAllGlobals();
});
