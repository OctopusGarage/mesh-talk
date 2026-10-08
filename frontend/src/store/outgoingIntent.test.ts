import { expect, it } from "vitest";
import { reconcileMessages } from "./outgoingIntent";
import type { ChatMessage } from "./chat";

const row = (id: string | null, clientId?: string): ChatMessage => ({
  id,
  clientId,
  fromMe: true,
  who: "me",
  text: "same text",
  wallClock: 1,
  replyTo: null,
});

it("reconciles an accepted intent only with its exact event ID", () => {
  const first = row("event-a");
  const second = row("event-b");
  const pending = row("event-b", "client-b");
  const result = reconcileMessages(
    [first, second],
    [pending],
    [
      {
        conversation: { kind: "account", id: "peer", name: "Peer" },
        payload: { kind: "text", text: "same text", replyTo: null },
        message: pending,
      },
    ],
    [],
  );
  expect(result).toHaveLength(2);
  expect(result[0].clientId).toBeUndefined();
  expect(result[1].clientId).toBe("client-b");
});
