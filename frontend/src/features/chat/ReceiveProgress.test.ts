import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import "@/lib/i18n";
import type { FileStatus } from "@/lib/types";
import { ReceiveProgress } from "./ReceiveProgress";

const render = (status?: FileStatus) =>
  renderToStaticMarkup(createElement(ReceiveProgress, { status }));
const receiving = (done: number, total: number): FileStatus => ({
  file_conv: "file",
  done,
  total,
  ready: false,
});

it.each([
  [0, 0],
  [-1, 10],
  [1, -1],
  [Number.NaN, 10],
  [1, Number.POSITIVE_INFINITY],
  [Number.POSITIVE_INFINITY, 10],
])("keeps unknown or invalid counts %s/%s indeterminate", (done, total) => {
  const html = render(receiving(done, total));
  expect(html).toContain('role="progressbar"');
  expect(html).not.toContain("aria-valuenow");
  expect(html).not.toContain("%");
  expect(html).not.toMatch(/NaN|Infinity/);
  expect(html).toContain("scaleX(0)");
});

it.each([
  [0, 0, 0],
  [4, 40, 0.4],
  [10, 100, 1],
  [12, 100, 1],
])("bounds rendered progress for %s of ten chunks", (done, percent, ratio) => {
  const html = render(receiving(done, 10));
  expect(html).toContain(`aria-valuenow="${percent}"`);
  expect(html).toContain(`>${percent}%</span>`);
  expect(html).toContain(`scaleX(${ratio})`);
});

it("keeps missing status indeterminate and ready attachments hidden", () => {
  expect(render()).not.toContain("aria-valuenow");
  expect(render({ ...receiving(10, 10), ready: true })).toBe("");
});

it.each(["pending", "declined"] as const)(
  "does not display byte progress while consent is %s",
  (consent) => {
    const html = render({ ...receiving(4, 10), consent });
    expect(html).toContain('role="status"');
    expect(html).not.toContain('role="progressbar"');
    expect(html).not.toContain("%");
  },
);
