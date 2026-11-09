// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { useVideoPoster } from "./mediaFile";
it("bounds failed video IDs and retries an evicted negative entry", async () => {
  (
    globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  const load = vi
    .spyOn(HTMLMediaElement.prototype, "load")
    .mockImplementation(() => {});
  const create = document.createElement.bind(document);
  const videos: HTMLVideoElement[] = [];
  const spy = vi
    .spyOn(document, "createElement")
    .mockImplementation((tag, options) => {
      const element = create(tag, options);
      if (tag === "video") videos.push(element as HTMLVideoElement);
      return element;
    });
  const root = createRoot(create("div"));
  function Probe({ id }: { id: string }) {
    useVideoPoster(`blob:${id}`, id);
    return null;
  }
  try {
    for (let i = 0; i < 65; i++) {
      await act(async () =>
        root.render(createElement(Probe, { id: `failed-${i}` })),
      );
      await act(async () =>
        videos[videos.length - 1]!.dispatchEvent(new Event("error")),
      );
    }
    const count = videos.length;
    await act(async () =>
      root.render(createElement(Probe, { id: "failed-63" })),
    );
    expect(videos.length).toBe(count);
    await act(async () =>
      root.render(createElement(Probe, { id: "failed-64" })),
    );
    expect(videos.length).toBe(count);
    await act(async () =>
      root.render(createElement(Probe, { id: "failed-0" })),
    );
    expect(videos.length).toBe(count + 1);
  } finally {
    await act(async () => root.unmount());
    spy.mockRestore();
    load.mockRestore();
  }
});
