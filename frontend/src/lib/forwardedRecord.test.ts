import { expect, it } from "vitest";
import { decodeForwardedRecord, isForwardedRecord } from "./forwardedRecord";
const valid = () => ({
  format: "MTCHAT1",
  title: "A conversation",
  created_at_ms: 1,
  messages: [
    {
      sender_name: "Alice",
      wall_clock: 1,
      text: "<script>literal</script>",
      file: {
        name: "photo.png",
        mime: "image/png",
        size: 1,
        data_base64: "YQ==",
      },
    },
  ],
});
const bytes = (value: unknown) =>
  new TextEncoder().encode(JSON.stringify(value));
it("decodes typed historical data and attachment bytes", () => {
  const result = decodeForwardedRecord(bytes(valid()));
  expect(result.messages[0].text).toBe("<script>literal</script>");
  expect([...result.messages[0].file!.bytes]).toEqual([97]);
  expect(isForwardedRecord("history.mtchat", "application/octet-stream")).toBe(
    true,
  );
  expect(isForwardedRecord("photo.png", "image/png")).toBe(false);
});
it.each([
  "version",
  "path",
  "base64",
  "size",
  "nested",
  "count",
  "sender",
  "utf8",
])("rejects invalid %s", (kind) => {
  const value = valid();
  if (kind === "version") value.format = "MTCHAT2";
  if (kind === "path") value.messages[0].file.name = "../photo.png";
  if (kind === "base64") value.messages[0].file.data_base64 = "YQ=!";
  if (kind === "size") value.messages[0].file.size = 100;
  if (kind === "nested") value.messages[0].file.name = "nested.mtchat";
  if (kind === "count")
    value.messages = Array.from({ length: 101 }, () => value.messages[0]);
  if (kind === "sender") value.messages[0].sender_name = "x".repeat(257);
  expect(() =>
    decodeForwardedRecord(
      kind === "utf8" ? new Uint8Array([255]) : bytes(value),
    ),
  ).toThrow();
});
it("rejects oversized input before decoding JSON", () => {
  expect(() =>
    decodeForwardedRecord(new Uint8Array(64 * 1024 * 1024 + 1)),
  ).toThrow();
});
