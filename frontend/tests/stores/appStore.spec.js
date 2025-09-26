import { beforeEach, describe, expect, it, vi } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { useAppStore } from "../../src/stores/appStore.js";
import { useFeedbackStore } from "../../src/stores/feedbackStore.js";

const { mockInvoke, mockListen } = vi.hoisted(() => ({
  mockInvoke: vi.fn(),
  mockListen: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mockInvoke,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: mockListen,
}));

const flushPromises = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("appStore real-time messaging integration", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    mockInvoke.mockReset();
    mockListen.mockReset();
    const feedback = useFeedbackStore();
    feedback.clearToasts();
    feedback.beginTask = vi.fn();
    feedback.endTask = vi.fn();
    feedback.showInfo = vi.fn();
  });

  it("refreshes store state when a message event arrives", async () => {
    const store = useAppStore();
    store.user = { id: 1, address: "self.node" };

    const inboundMessage = {
      id: 42,
      from_address: "peer.node",
      to_address: "self.node",
      to_user_id: 1,
      status: 0,
      content: "hello there",
    };

    mockInvoke.mockImplementation((command, payload) => {
      switch (command) {
        case "get_messages":
          return Promise.resolve([inboundMessage]);
        case "mark_message_read":
          return Promise.resolve({
            ...inboundMessage,
            status: 2,
            read_at: Date.now(),
          });
        default:
          return Promise.resolve({ success: true });
      }
    });

    const handlers = {};
    mockListen.mockImplementation((eventName, handler) => {
      handlers[eventName] = handler;
      return Promise.resolve(() => {});
    });

    store.ensureEventListeners();
    await flushPromises();

    await handlers["message-received"]({
      payload: {
        message: inboundMessage,
        sender_address: "peer.node",
        sender_name: "Peer Node",
      },
    });
    await flushPromises();

    expect(mockInvoke).toHaveBeenCalledWith("mark_message_read", {
      messageId: 42,
    });
    expect(store.messages).toHaveLength(1);
    expect(store.messages[0].status).toBe(2);
    expect(store.activeConversation).toBe("peer.node");
    expect(store.contacts[0].address).toBe("peer.node");
    expect(store.unreadCount).toBe(0);
  });

  it("updates network status and tears down listeners", async () => {
    const store = useAppStore();

    const handlers = {};
    const unlistenSpy = vi.fn();

    mockInvoke.mockResolvedValue({ success: true });
    mockListen.mockImplementation((eventName, handler) => {
      handlers[eventName] = handler;
      return Promise.resolve(unlistenSpy);
    });

    store.ensureEventListeners();
    await flushPromises();

    await handlers["network-status-changed"]({
      payload: { status: "connected", peer_count: 2 },
    });

    expect(store.networkStatus).toBe("connected");
    expect(store.peerCount).toBe(2);

    await store.teardownEventListeners();
    expect(unlistenSpy).toHaveBeenCalledTimes(4);
  });

  it("updates node info when the port changes", async () => {
    const store = useAppStore();

    const handlers = {};
    mockInvoke.mockResolvedValue({ success: true });
    mockListen.mockImplementation((eventName, handler) => {
      handlers[eventName] = handler;
      return Promise.resolve(() => {});
    });

    store.ensureEventListeners();
    await flushPromises();

    await handlers["node-port-changed"]({ payload: { port: 8123 } });
    expect(store.nodeInfo?.port).toBe(8123);

    await handlers["node-port-changed"]({ payload: { port: 9000 } });
    expect(store.nodeInfo?.port).toBe(9000);

    await store.teardownEventListeners();
  });

  it("updates contact details via updateContact", async () => {
    const store = useAppStore();
    store.user = { id: 1, address: "self.node" };

    const updatedContact = {
      id: 1,
      name: "Bobby",
      address: "10.0.0.5:7000",
      status: "offline",
      is_online: false,
      added_at: Date.now(),
      notes: "Runner",
    };

    mockInvoke.mockImplementation((command, payload) => {
      switch (command) {
        case "update_contact":
          expect(payload).toEqual({
            id: 1,
            name: "Bobby",
            address: undefined,
            notes: "Runner",
          });
          return Promise.resolve({
            success: true,
            contact: updatedContact,
            contacts: [updatedContact],
          });
        default:
          return Promise.resolve({ success: true });
      }
    });

    store.contacts = [
      {
        id: 1,
        name: "Bob",
        address: "10.0.0.5:7000",
        status: "offline",
        is_online: false,
        added_at: Date.now(),
        notes: null,
      },
    ];

    const result = await store.updateContact(1, {
      name: "Bobby",
      notes: "Runner",
    });
    expect(result.success).toBe(true);
    expect(store.contacts[0].name).toBe("Bobby");
    expect(store.contacts[0].notes).toBe("Runner");
  });
});
