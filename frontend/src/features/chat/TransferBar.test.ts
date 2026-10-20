import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import "@/lib/i18n";
import type { Transfer } from "@/store/transfers";
import { TransferBar } from "./TransferBar";

const state = vi.hoisted(() => ({
  transfer: undefined as Transfer | undefined,
}));
vi.mock("@/store/transfers", () => ({ useTransfer: () => state.transfer }));

const render = (waitingDirection?: "prepare" | "send") =>
  renderToStaticMarkup(
    createElement(TransferBar, { transferKey: "file", waitingDirection }),
  );
const transfer = (done: number, total: number): Transfer => ({
  fileConv: "file",
  direction: "send",
  done,
  total,
  etaSeconds: 42,
});

afterEach(() => {
  state.transfer = undefined;
});

it("announces preparation without inventing a percentage", () => {
  const html = render("prepare");
  expect(html).toContain('role="progressbar"');
  expect(html).toContain("Preparing");
  expect(html).not.toContain("%");
  expect(html).not.toContain("aria-valuenow");
});

it.each([
  [0, 0],
  [1, -1],
  [-1, 10],
  [Number.NaN, 10],
  [1, Number.POSITIVE_INFINITY],
  [Number.POSITIVE_INFINITY, 10],
])("keeps invalid or unknown counts %s/%s indeterminate", (done, total) => {
  state.transfer = transfer(done, total);
  const html = render();
  expect(html).not.toContain("%");
  expect(html).not.toContain("aria-valuenow");
  expect(html).not.toContain("42");
  expect(html).not.toMatch(/NaN|Infinity/);
});

it.each([
  [0, 0],
  [4, 40],
  [10, 100],
  [12, 100],
])("shows observed progress for %s of ten bytes", (done, percentage) => {
  state.transfer = transfer(done, 10);
  const html = render();
  expect(html).toContain(`aria-valuenow="${percentage}"`);
  expect(html).toContain(`>${percentage}%</span>`);
  if (done >= 10) expect(html).toContain("Confirming");
});

it("renders nothing for inactive or delivered transfers", () => {
  expect(render()).toBe("");
  state.transfer = { ...transfer(10, 10), delivered: true };
  expect(render()).toBe("");
});

it("shows an ETA only during measured, incomplete progress", () => {
  state.transfer = transfer(4, 10);
  expect(render()).toContain("42s left");
  for (const etaSeconds of [Number.NaN, Number.POSITIVE_INFINITY, -1]) {
    state.transfer = { ...transfer(4, 10), etaSeconds };
    expect(render()).not.toContain(" left</div>");
  }
  for (const done of [0, 10]) {
    state.transfer = transfer(done, 10);
    expect(render()).not.toContain(" left</div>");
  }
});
