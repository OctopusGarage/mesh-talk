// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it } from "vitest";
import { useAsyncFeedback } from "./useAsyncFeedback";

let feedback: ReturnType<typeof useAsyncFeedback>;
const container = document.createElement("div");
const root = createRoot(container);

function Harness() {
  feedback = useAsyncFeedback();
  return null;
}

afterEach(async () => {
  await act(async () => root.render(null));
});

it("ignores a completed request after its feedback is reset", async () => {
  await act(async () => root.render(createElement(Harness)));
  let resolve!: () => void;
  const request = new Promise<void>((done) => {
    resolve = done;
  });
  await act(async () => {
    void feedback.run(() => request);
  });
  expect(feedback.busy).toBe(true);
  await act(async () => feedback.reset());
  await act(async () => resolve());
  expect(feedback).toMatchObject({
    busy: false,
    succeeded: false,
    error: null,
  });
});

it("only reports the latest result when requests finish out of order", async () => {
  await act(async () => root.render(createElement(Harness)));
  let rejectOld!: (reason: Error) => void;
  const old = new Promise<void>((_, reject) => {
    rejectOld = reject;
  });
  await act(async () => {
    void feedback.run(() => old);
  });
  await act(async () => {
    void feedback.run(async () => {});
  });
  await act(async () => rejectOld(new Error("stale failure")));
  expect(feedback).toMatchObject({ busy: false, succeeded: true, error: null });
});
