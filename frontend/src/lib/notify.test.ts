import { describe, it, expect } from "vitest";
import { shouldNotify } from "./notify";

describe("shouldNotify", () => {
  it("suppresses when focused on the active conversation", () => {
    expect(
      shouldNotify({ windowFocused: true, isActiveConversation: true }),
    ).toBe(false);
  });
  it("notifies when the window is unfocused", () => {
    expect(
      shouldNotify({ windowFocused: false, isActiveConversation: true }),
    ).toBe(true);
  });
  it("notifies when focused but a different conversation is open", () => {
    expect(
      shouldNotify({ windowFocused: true, isActiveConversation: false }),
    ).toBe(true);
  });
});
