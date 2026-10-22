import { describe, expect, it } from "vitest";
import { connectionState } from "./connectionState";

describe("connectionState", () => {
  it("separates node failure from a healthy search with no peers", () => {
    expect(
      connectionState({
        bootFailed: true,
        ready: false,
        noNetwork: false,
        onlinePeople: 0,
      }),
    ).toBe("failed");
    expect(
      connectionState({
        bootFailed: false,
        ready: true,
        noNetwork: false,
        onlinePeople: 0,
      }),
    ).toBe("searching");
  });

  it("puts startup and missing network ahead of peer count", () => {
    expect(
      connectionState({
        bootFailed: false,
        ready: false,
        noNetwork: true,
        onlinePeople: 2,
      }),
    ).toBe("starting");
    expect(
      connectionState({
        bootFailed: false,
        ready: true,
        noNetwork: true,
        onlinePeople: 2,
      }),
    ).toBe("no-network");
    expect(
      connectionState({
        bootFailed: false,
        ready: true,
        noNetwork: false,
        onlinePeople: 2,
      }),
    ).toBe("ready");
  });
});

it("shows reconnecting instead of a new network search while recent peers recover", () => {
  expect(
    connectionState({
      bootFailed: false,
      ready: true,
      noNetwork: false,
      onlinePeople: 0,
      reconnecting: true,
    }),
  ).toBe("reconnecting");
  expect(
    connectionState({
      bootFailed: false,
      ready: true,
      noNetwork: false,
      onlinePeople: 1,
      reconnecting: true,
    }),
  ).toBe("ready");
  expect(
    connectionState({
      bootFailed: false,
      ready: true,
      noNetwork: true,
      onlinePeople: 0,
      reconnecting: true,
    }),
  ).toBe("no-network");
});
