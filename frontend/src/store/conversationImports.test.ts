import { readFileSync } from "node:fs";
import { expect, expectTypeOf, it } from "vitest";
import type { SendIntent, SendPayload } from "./conversationModel";
import type {
  SendIntent as CompatibleSendIntent,
  SendPayload as CompatibleSendPayload,
} from "./outgoingIntent";

const source = (name: string) =>
  readFileSync(new URL(`./${name}.ts`, import.meta.url), "utf8");

it("owns shared send types in the model without a state-to-dispatch dependency", () => {
  const model = source("conversationModel");
  expect(model).toMatch(/export interface SendIntent\b/);
  expect(model).toMatch(/export type SendPayload\b/);
  expect(source("conversationState")).not.toMatch(
    /from ["']\.\/outgoingIntent["']/,
  );
  expect(source("outgoingIntent")).toMatch(
    /export type \{ SendIntent, SendPayload \} from ["']\.\/conversationModel["']/,
  );
  expectTypeOf<SendIntent>().toEqualTypeOf<CompatibleSendIntent>();
  expectTypeOf<SendPayload>().toEqualTypeOf<CompatibleSendPayload>();
});

it("imports narrow IPC adapters inside conversation stores", () => {
  for (const name of ["outgoingIntent", "conversationHistory", "chat"]) {
    expect(source(name)).not.toMatch(/from ["']@\/lib\/api["']/);
    expect(source(name)).toMatch(/from ["']@\/lib\/api\/chat["']/);
  }
  expect(source("chat")).toMatch(/from ["']@\/lib\/api\/contacts["']/);
});
