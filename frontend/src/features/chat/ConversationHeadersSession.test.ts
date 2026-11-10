// @vitest-environment jsdom
import "@/lib/i18n";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { useAuth } from "@/store/auth";
import { registerRuntimeSnapshot } from "@/store/ownership";
const runtime = vi.hoisted(() => ({ runEpoch: 1, identityEpoch: 1 }));
const api = vi.hoisted(() => ({ getTrust: vi.fn() }));
vi.mock("@/lib/api", () => ({ chat: api }));
vi.mock("@/store/chat", () => ({
  useChat: (selector: (state: unknown) => unknown) =>
    selector({
      ...runtime,
      peers: [{ account_id: "alice", user_id: "device-alice" }],
    }),
}));
vi.mock("@/store/presence", () => ({
  usePresenceFor: () => null,
  presenceStatus: () => "offline",
  presenceLabel: () => "Offline",
}));
vi.mock("@/components/identity", () => ({
  IdentityCrest: ({ verified }: { verified: boolean }) =>
    createElement("span", { "data-verified": String(verified) }),
  AvatarEditMenu: () => null,
}));
vi.mock("./MemberProfileDialog", () => ({ MemberProfileDialog: () => null }));
vi.mock("./PersonalDevices", () => ({ PersonalDevices: () => null }));
import { DmHeader } from "./ConversationHeaders";
it("rejects a pending verified crest response after the same owner signs in again", async () => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  useAuth.setState({
    user: { id: "owner", username: "owner", display_name: "Owner" },
    generation: 1,
  });
  let finish!: (value: unknown) => void;
  api.getTrust
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockImplementation(() => new Promise(() => {}));
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  try {
    await act(async () =>
      root.render(createElement(DmHeader, { id: "alice", name: "Alice" })),
    );
    await act(async () => {
      useAuth.setState({ generation: 2 });
      finish({ verified: true, fingerprint_changed: false });
    });
    expect(
      host.querySelector("[data-verified]")?.getAttribute("data-verified"),
    ).toBe("false");
    expect(api.getTrust).toHaveBeenCalledTimes(2);
  } finally {
    await act(async () => root.unmount());
    host.remove();
  }
});

it("retries a trust read after node identity recovery with unchanged owner and fingerprint", async () => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  useAuth.setState({
    user: { id: "owner", username: "owner", display_name: "Owner" },
    generation: 1,
  });
  runtime.runEpoch = 1;
  runtime.identityEpoch = 1;
  registerRuntimeSnapshot(() => ({ ...runtime }));
  api.getTrust.mockReset();
  let finish!: (value: unknown) => void;
  api.getTrust
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValue({ verified: true, fingerprint_changed: false });
  const host = document.createElement("div");
  document.body.append(host);
  const root = createRoot(host);
  try {
    await act(async () =>
      root.render(createElement(DmHeader, { id: "alice", name: "Alice" })),
    );
    await act(async () => {
      runtime.identityEpoch++;
      finish({ verified: false, fingerprint_changed: false });
      root.render(createElement(DmHeader, { id: "alice", name: "Alice" }));
    });
    expect(api.getTrust).toHaveBeenCalledTimes(2);
    expect(
      host.querySelector("[data-verified]")?.getAttribute("data-verified"),
    ).toBe("true");
  } finally {
    await act(async () => root.unmount());
    host.remove();
    registerRuntimeSnapshot(() => ({ runEpoch: 0, identityEpoch: 0 }));
  }
});
