// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { expect, it } from "vitest";
import { useCopyFeedback } from "./useCopyFeedback";

it("does not show stale copy feedback after a newer copy starts", async () => {
  let finishFirst!: () => void;
  const first = new Promise<void>((resolve) => {
    finishFirst = resolve;
  });
  let copy!: ReturnType<typeof useCopyFeedback>["copy"];
  let status!: ReturnType<typeof useCopyFeedback>["status"];
  function Probe() {
    ({ copy, status } = useCopyFeedback());
    return null;
  }
  const root = createRoot(document.createElement("div"));
  await act(async () => root.render(createElement(Probe)));
  let pendingFirst!: Promise<void>;
  await act(async () => {
    pendingFirst = copy(() => first);
    await copy(() => Promise.reject(new Error("clipboard unavailable")));
  });
  expect(status).toBe("failed");
  await act(async () => {
    finishFirst();
    await pendingFirst;
  });
  expect(status).toBe("failed");
  await act(async () => root.unmount());
});
