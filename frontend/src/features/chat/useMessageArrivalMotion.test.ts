// @vitest-environment jsdom
import { act, createElement, useLayoutEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it } from "vitest";
import type { ChatMessage } from "@/store/chat";
import { useMessageArrivalMotion } from "./useMessageArrivalMotion";

const message = (
  id: string,
  extra: Partial<ChatMessage> = {},
): ChatMessage => ({
  id,
  fromMe: false,
  who: "Alice",
  text: id,
  wallClock: 1,
  replyTo: null,
  ...extra,
});
let root: Root;
let container: HTMLDivElement;
let freshness: boolean[];
let arrival: ReturnType<typeof useMessageArrivalMotion>;
function Harness({
  scope,
  rows,
  context,
  mount = true,
}: {
  scope: string;
  rows: ChatMessage[];
  context: boolean;
  mount?: boolean;
}) {
  const fresh = useMessageArrivalMotion(scope, rows, context);
  arrival = fresh;
  freshness = rows.map(fresh);
  useLayoutEffect(() => {
    if (mount) rows.forEach(fresh.mounted);
  });

  return null;
}
async function render(
  rows: ChatMessage[],
  scope = "a",
  context = false,
  mount = true,
) {
  await act(async () =>
    root.render(createElement(Harness, { scope, rows, context, mount })),
  );
  return freshness;
}
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  container = document.createElement("div");
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
});

it("never treats unclassified history, pagination or catch-up as new arrivals", async () => {
  await render([message("baseline")]);
  expect(
    await render([
      message("history"),
      message("baseline"),
      message("catchup", { arrivalKind: "catch_up" }),
    ]),
  ).toEqual([false, false, false]);
});
it("gates the first render of another conversation synchronously", async () => {
  await render([message("baseline")]);
  expect(
    await render([message("other", { arrivalKind: "live" })], "b"),
  ).toEqual([false]);
});
it("never animates search context, even if it retains live provenance", async () => {
  await render([message("baseline")]);
  expect(
    await render([message("context", { arrivalKind: "live" })], "a", true),
  ).toEqual([false]);
});
it("animates live and locally admitted pending messages only once", async () => {
  const baseline = message("baseline");
  const incoming = message("new", { arrivalKind: "live" });
  const outgoing = message("pending", {
    id: null,
    fromMe: true,
    pending: true,
    clientId: "local-intent",
  });
  await render([baseline]);
  expect(await render([baseline, incoming, outgoing])).toEqual([
    false,
    true,
    true,
  ]);
  expect(await render([baseline, incoming, outgoing])).toEqual([
    false,
    false,
    false,
  ]);
  await render([baseline]);
  expect(await render([baseline, incoming, outgoing])).toEqual([
    false,
    false,
    false,
  ]);
});

it("keeps delayed virtual mounts eligible and consumes their motion once committed", async () => {
  await render([message("baseline")]);
  const live = message("delayed", { arrivalKind: "live" });
  await render([message("baseline"), live], "a", false, false);
  await render([message("baseline"), live], "a", false, false);
  expect(arrival(live, 1)).toBe(true);
  arrival.mounted(live, 1);
  expect(arrival(live, 1)).toBe(false);
});
it("does not replay live messages received while reading older history", async () => {
  await render([message("baseline")]);
  const live = message("offscreen", { arrivalKind: "live" });
  await render([message("baseline"), live], "a", true, false);
  expect(await render([message("baseline"), live])).toEqual([false, false]);
});
