import { defineStore } from "pinia";
import { computed, reactive, ref, watch } from "vue";
import { listen } from "@tauri-apps/api/event";
import { API } from "../services/api";
import { useFeedbackStore } from "./feedbackStore";
import {
  normalizeMessage,
  normalizeMessageList,
  buildNodeDisplayLabel,
  splitAddress,
  getMessageConversationKey,
  buildDiscoveredLabel
} from "../utils/addressUtils";

export const useAppStore = defineStore("app", () => {
  // Dependencies
  const feedback = useFeedbackStore();
  
  // State
  const user = ref(null);
  const nodeInfo = ref(null);
  const contacts = ref([]);
  const messages = ref([]);
  const networkStatus = ref("disconnected");
  const peerCount = ref(0);
  const networkRunning = ref(false);
  const unreadCount = ref(0);
  const loading = ref(false);
  const error = ref(null);
  const listeners = reactive([]);
  const activeConversation = ref(null);
  const discoveredNodes = ref([]); // Added: List of discovered network nodes

  // Computed properties
  const isAuthenticated = computed(() => Boolean(user.value));
  
  const sortedContacts = computed(() =>
    [...contacts.value].sort((a, b) => a.name.localeCompare(b.name)),
  );
  
  const filteredMessages = computed(() => {
    if (!activeConversation.value) {
      return messages.value;
    }

    return messages.value.filter(
      (message) => messageConversationKey(message) === activeConversation.value,
    );
  });

  

  // Contact normalization functions
  function normalizeContact(contact) {
    if (!contact) {
      return contact;
    }

    const status = contact.status ?? (contact.is_online ? "online" : "offline");
    const [addressIp, addressPort] = splitAddress(contact.address ?? "");

    const accountName = user.value?.name ?? "Guest";
    const nodeName = contact.node_name ?? contact.name ?? "Unknown";
    const username = contact.username ?? "Unknown";
    const listenPort = contact.listen_port ?? addressPort ?? 0;
    const displayLabel =
      contact.display_label ??
      buildNodeDisplayLabel({
        name: nodeName,
        username: accountName,
        ip: contact.ip ?? addressIp ?? "127.0.0.1",
        port: listenPort,
      });

    return {
      ...contact,
      node_name: nodeName,
      username,
      listen_port: listenPort,
      ip: contact.ip ?? addressIp ?? "127.0.0.1",
      display_label: displayLabel,
      name: displayLabel,
      status,
      is_online: contact.is_online ?? status === "online",
      added_at: contact.added_at ?? Date.now(),
    };
  }

  function applyContacts(list = []) {
    const normalized = [...list]
      .map(normalizeContact)
      .sort((a, b) =>
        a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
      );
    contacts.value = normalized;
    return normalized;
  }

  // Message normalization functions
  

  function upsertMessage(message) {
    if (!message) {
      return null;
    }

    const normalized = normalizeMessage(message);
    
    // Prevent duplicates based on ID or content/sender/timestamp combination
    const current = Array.isArray(messages.value) ? [...messages.value] : [];
    
    // First, try to find by ID
    let index = current.findIndex((entry) => entry.id === normalized.id && normalized.id !== 0);
    
    if (index === -1) {
      // If no ID match (or ID is 0), try to find potential duplicate by content, sender, and time
      index = current.findIndex((entry) => 
        entry.content === normalized.content &&
        entry.from_address === normalized.from_address &&
        Math.abs((entry.sent_at || 0) - (normalized.sent_at || 0)) < 2 // Within 2 seconds
      );
    }

    if (index === -1) {
      current.push(normalized);
    } else {
      current.splice(index, 1, normalized);
    }

    messages.value = normalizeMessageList(current);
    return normalized;
  }

  // Message conversation key function
  

  // Message conversation key function
  function messageConversationKey(message) {
    if (!message) {
      return null;
    }

    const selfAddress = user.value?.address;

    // If this message is from the current user, return the recipient address
    if (message.from_address === selfAddress && message.to_address) {
      return message.to_address;
    }
    
    // If this message is from someone else, return the sender address
    if (message.from_address && message.from_address !== selfAddress) {
      return message.from_address;
    }

    // Fallback to to_address if from_address doesn't match self
    if (message.to_address && message.to_address !== selfAddress) {
      return message.to_address;
    }

    return null;
  }

  // State management utility functions
  function setLoading(state) {
    loading.value = state;
  }

  function setError(err, options = {}) {
    if (!err) {
      error.value = null;
      if (options.clearLastError) {
        feedback.clearLastError();
      }
      return;
    }

    console.error(err);
    const message =
      options.message ||
      (typeof err === "string" ? err : (err?.message ?? "Unknown error"));

    error.value = message;

    feedback.showError(err, {
      message,
      title: options.title,
      toast: options.toast !== false,
      context: options.source ?? "appStore",
    });
  }

  // Authentication actions
  async function login(username, password) {
    setLoading(true);
    setError(null, { clearLastError: true });
    const taskKey = feedback.beginTask("auth:login", "Signing in…");
    try {
      const result = await API.auth.login(username, password);
      if (!result.success) {
        throw new Error(result.error ?? "Unable to login");
      }
      cleanup(); // Clean up any existing intervals before setting up new ones
      user.value = result.user;
      await bootstrapAfterAuth();
      feedback.showSuccess(`Signed in as ${result.user?.name ?? username}`, {
        autoDismiss: 3200,
      });
      return { success: true };
    } catch (err) {
      setError(err, {
        message: "Unable to login",
        source: "auth.login",
      });
      return { success: false, error: error.value };
    } finally {
      setLoading(false);
      feedback.endTask(taskKey);
    }
  }

  async function register(username, password) {
    setLoading(true);
    setError(null, { clearLastError: true });
    const taskKey = feedback.beginTask("auth:register", "Creating account…");
    try {
      const result = await API.auth.register(username, password);
      if (!result.success) {
        throw new Error(result.error ?? "Registration failed");
      }

      feedback.showSuccess("Account created", {
        detail: `Sign in as ${result.user?.name ?? username}`,
      });

      return { success: true, user: result.user };
    } catch (err) {
      setError(err, {
        message: "Registration failed",
        source: "auth.register",
      });
      return { success: false, error: error.value };
    } finally {
      setLoading(false);
      feedback.endTask(taskKey);
    }
  }

  async function logout() {
    setLoading(true);
    setError(null, { clearLastError: true });
    const taskKey = feedback.beginTask("auth:logout", "Signing out…");
    try {
      const result = await API.auth.logout();
      if (!result.success) {
        throw new Error(result.error ?? "Logout failed");
      }
      feedback.showInfo("Signed out", { autoDismiss: 2500 });
    } catch (err) {
      setError(err, {
        message: "Logout failed",
        source: "auth.logout",
      });
    } finally {
      teardownEventListeners();
      cleanup(); // Clean up intervals
      user.value = null;
      nodeInfo.value = null;
      contacts.value = [];
      messages.value = [];
      networkStatus.value = "disconnected";
      networkRunning.value = false;
      peerCount.value = 0;
      unreadCount.value = 0;
      activeConversation.value = null;
      setLoading(false);
      feedback.endTask(taskKey);
    }
  }

  // Data refresh actions
  async function bootstrapAfterAuth() {
    await Promise.all([
      refreshContacts(),
      refreshMessages(),
      refreshNodeInfo(),
      refreshDiscoveredNodes(),
    ]);
    ensureEventListeners();
  }

  async function refreshNodeInfo() {
    try {
      const info = await API.node.getNodeInfo();
      nodeInfo.value = {
        ...info,
        display_label:
          info.display_label ??
          buildNodeDisplayLabel({
            name: info.name,
            username: info.username ?? user.value?.name ?? "Guest",
            ip: info.ip,
            port: info.port,
          }),
      };
      if (info.status && info.status.toLowerCase() !== "offline") {
        networkRunning.value = true;
        networkStatus.value = "connected";
      }
    } catch (err) {
      setError(err, {
        message: "Failed to load node information",
        source: "store.refreshNodeInfo",
        toast: false,
      });
    }
  }

  async function refreshDiscoveredNodes() {
    // This function is now handled by the nodes-discovered event
    // We keep it for compatibility but it doesn't do anything
    return Promise.resolve();
  }

  async function refreshContacts() {
    try {
      const result = await API.contacts.getContacts();
      if (result.success) {
        applyContacts(result.contacts ?? []);
      }
    } catch (err) {
      setError(err, {
        message: "Failed to refresh contacts",
        source: "store.refreshContacts",
        toast: false,
      });
    }
  }

  async function refreshMessages() {
    try {
      const result = await API.messages.getMessages();
      const normalized = normalizeMessageList(Array.isArray(result) ? result : []);
      messages.value = normalized;
      recomputeUnread();
    } catch (err) {
      setError(err, {
        message: "Failed to refresh messages",
        source: "store.refreshMessages",
        toast: false,
      });
    }
  }

  // Message actions
  async function sendMessage(content) {
    if (activeConversation.value) {
      await ensureNodeConnection(activeConversation.value);
    }
    setLoading(true);
    setError(null, { clearLastError: true });
    try {
      // Prepare the message object to be stored locally before sending
      const localMessage = {
        id: Date.now(), // Temporary ID until backend assigns real ID
        from_user_id: user.value?.id,
        from_address: user.value?.address,
        to_address: activeConversation.value,
        content: content,
        sent_at: Math.floor(Date.now() / 1000),
        status: 0, // Sent (not yet delivered)
      };
      
      // Add the local message immediately to show in UI
      const addedMessage = upsertMessage(localMessage);
      
      // Now send to backend
      const result = await API.messages.sendMessage(content);
      
      // Merge the backend result with the local one (in case backend returns additional fields)
      let processedResult = { ...localMessage, ...result };
      
      const normalized = upsertMessage(processedResult) ?? normalizeMessage(processedResult);
      const key = messageConversationKey(normalized);
      if (!activeConversation.value && key) {
        activeConversation.value = key;
      }
      return { success: true, message: normalized };
    } catch (err) {
      setError(err, {
        message: "Failed to send message",
        source: "messages.send",
      });
      return { success: false, error: error.value };
    } finally {
      setLoading(false);
    }
  }

  async function markMessageRead(messageId) {
    try {
      const result = await API.messages.markMessageRead(messageId);
      const index = messages.value.findIndex((msg) => msg.id === result.id);
      if (index !== -1) {
        messages.value[index] = result;
      }
      recomputeUnread();
    } catch (err) {
      setError(err, {
        message: "Failed to update message",
        source: "messages.markRead",
        toast: false,
      });
    }
  }

  async function markAllMessagesRead() {
    try {
      await API.messages.markAllMessagesRead();
      messages.value = messages.value.map((msg) => ({
        ...msg,
        status: 2,
        read_at: msg.read_at ?? Date.now(),
      }));
      recomputeUnread();
    } catch (err) {
      setError(err, {
        message: "Failed to mark messages read",
        source: "messages.markAllRead",
        toast: false,
      });
    }
  }

  // Contact management actions
  async function updateContact(contactId, data) {
    try {
      const result = await API.contacts.updateContact(contactId, data);
      
      // Update the contact in the local store if successful
      if (result.success && result.contact) {
        const contactIndex = contacts.value.findIndex(contact => contact.id === contactId);
        if (contactIndex !== -1) {
          // Preserve the original name if it was explicitly provided in the update
          const updatedContact = {
            ...contacts.value[contactIndex],
            ...result.contact
          };
          
          contacts.value[contactIndex] = normalizeContact(updatedContact);
          
          // If name was explicitly provided in the update, override the normalized name field
          if (data.name !== undefined) {
            contacts.value[contactIndex].name = data.name;
          }
        } else {
          // If contact doesn't exist locally, add it
          const newContact = normalizeContact(result.contact);
          // If name was explicitly provided, override the normalized name field
          if (data.name !== undefined) {
            newContact.name = data.name;
          }
          contacts.value.push(newContact);
        }
      }
      
      return result;
    } catch (err) {
      setError(err, {
        message: "Failed to update contact",
        source: "contacts.update",
        toast: true,
      });
      return { success: false, error: err.message };
    }
  }

  // Conversation and connection actions
  function selectConversation(address) {
    activeConversation.value = address;
  }

  async function ensureNodeConnection(address) {
    const target = address?.trim();
    if (!target) {
      return;
    }
    try {
      await API.node.connectToNode(target);
    } catch (err) {
      setError(err, {
        message: "Failed to connect to node",
        source: "network.connect",
        toast: true,
      });
    }
  }

  // Event listener management
  function ensureEventListeners() {
    if (listeners.length > 0) {
      return;
    }

    listeners.push(
      listen("message-received", async (event) => {
        const payload = event.payload ?? {};
        const incomingMessage = payload.message;

        if (incomingMessage) {
          const normalized = upsertMessage(incomingMessage) ?? normalizeMessage(incomingMessage);

          if (payload.sender_address) {
            upsertContactFromEvent({
              address: payload.sender_address,
              status: "online",
              name: payload.sender_name ?? payload.sender_address,
              contact_id: payload.contact_id,
            });
          }

          const conversationKey = messageConversationKey(normalized);

          if (!activeConversation.value && conversationKey) {
            activeConversation.value = conversationKey;
          }

          if (conversationKey && activeConversation.value === conversationKey) {
            if (normalized.id) {
              await autoMarkConversationRead(conversationKey);
            }
          } else {
            recomputeUnread();
          }
        } else {
          await refreshMessages();
        }
      }),
      listen("contact-status-changed", (event) => {
        const payload = event.payload ?? {};
        upsertContactFromEvent(payload);
      }),
      listen("contact-added", async (event) => {
        const payload = event.payload ?? {};
        if (payload.public_key) {
          upsertContactFromEvent({
            address: payload.public_key,
            node_name: payload.alias,
            username: payload.alias,
            status: "online",
          });
        }
        await refreshContacts();
      }),
      listen("network-status-changed", (event) => {
        const payload = event.payload ?? {};
        const incoming = (payload.status ?? "disconnected").toLowerCase();
        let nextStatus = incoming;
        if (incoming === "disconnected" && networkRunning.value) {
          nextStatus = "connected";
        } else if (incoming === "online") {
          networkRunning.value = true;
          nextStatus = "connected";
        }
        networkStatus.value = nextStatus;
        peerCount.value = payload.peer_count ?? 0;
      }),
      listen("node-port-changed", (event) => {
        const payload = event.payload ?? {};
        const current = nodeInfo.value ?? {};
        const updatedPort = payload.port ?? current.port ?? 0;
        const updatedIp = payload.ip ?? current.ip ?? "127.0.0.1";
        const updatedName = current.name ?? "mesh-node";
        const updatedUsername =
          current.username ?? user.value?.name ?? "Guest";

        nodeInfo.value = {
          ...current,
          name: updatedName,
          username: updatedUsername,
          ip: updatedIp,
          port: updatedPort,
          status: current.status ?? (user.value ? "Online" : "Offline"),
          peer_count: current.peer_count ?? peerCount.value ?? 0,
          display_label: buildNodeDisplayLabel({
            name: updatedName,
            username: updatedUsername,
            ip: updatedIp,
            port: updatedPort,
          }),
        };
        networkRunning.value = true;
        networkStatus.value = "connected";
        if (payload.port) {
          feedback.showInfo(`Listening on TCP port ${payload.port}`, {
            autoDismiss: 2800,
          });
        }
      }),
      // Listen for network discovery events (keeping hooks for future expansion, but data is unified via nodes-discovered)
      listen("node-discovered", () => {
        // Discovery list is authoritative via nodes-discovered; ignore incremental pushes.
      }),
      // Listen for node discovery events
      listen("nodes-discovered", (event) => {
        const payload = event.payload ?? {};
        if (payload.nodes) {
          discoveredNodes.value = payload.nodes.map((node) => ({
            ...node,
            display_label:
              node.display_label ??
              buildNodeDisplayLabel({
                name: node.name,
                username: node.username ?? "Unknown",
                ip: node.ip ?? splitAddress(node.address)[0],
                port: node.listen_port ?? node.port ?? undefined,
              }),
          }));
        }
      })
    );
  }

  async function teardownEventListeners() {
    const pending = listeners.splice(0, listeners.length);
    for (const maybePromise of pending) {
      const unlisten = await maybePromise;
      if (typeof unlisten === "function") {
        unlisten();
      }
    }
  }

  // Contact management
  function upsertContactFromEvent(payload) {
    if (!payload.address) {
      return;
    }

    const status = payload.status ?? "offline";
    const [addressIp, addressPort] = splitAddress(payload.address ?? "");

    const accountName = user.value?.name ?? "Guest";
    const nodeName = payload.node_name ?? payload.name ?? "Unknown";
    const username = payload.username ?? "Unknown";
    const listenPort = payload.listen_port ?? addressPort ?? 0;
    const displayLabel =
      payload.display_label ??
      buildNodeDisplayLabel({
        name: nodeName,
        username: accountName,
        ip: payload.ip ?? addressIp ?? "127.0.0.1",
        port: listenPort,
      });
    const id = payload.contact_id ?? null;

    const existingIndex = contacts.value.findIndex(
      (contact) => contact.address === payload.address || contact.id === id,
    );

    const existing =
      existingIndex !== -1 ? contacts.value[existingIndex] : null;
    if (existingIndex === -1 && id === null) {
      // Without a contact identifier we avoid mutating discovery state here; UDP discovery owns that list.
      // But we still need to handle status updates for contacts that might exist but weren't explicitly identified
      // Check if this address exists in the contacts list by comparing address
      const contactIndexByAddress = contacts.value.findIndex(
        contact => contact.address === payload.address
      );
      
      if (contactIndexByAddress !== -1) {
        // Update the contact status even if we don't have the contact_id
        const existingContact = contacts.value[contactIndexByAddress];
        contacts.value[contactIndexByAddress] = {
          ...existingContact,
          status,
          is_online: status === "online",
        };
      }
      return;
    }
    const contactId = existing?.id ?? id ?? Math.trunc(Date.now());
    const normalized = normalizeContact({
      id: contactId,
      node_name: nodeName,
      username,
      listen_port: listenPort,
      ip: payload.ip ?? addressIp ?? "127.0.0.1",
      display_label: displayLabel,
      name: displayLabel,
      address: payload.address,
      status,
      is_online: status === "online",
      added_at: existing?.added_at ?? Date.now(),
      notes: existing?.notes ?? null,
    });

    if (existingIndex !== -1) {
      contacts.value[existingIndex] = {
        ...existing,
        ...normalized,
      };
    } else {
      contacts.value.push(normalized);
    }

    contacts.value = [...contacts.value]
      .map(normalizeContact)
      .sort((a, b) =>
        a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
      );
  }

  // Unread message tracking
  function recomputeUnread() {
    unreadCount.value = messages.value.filter(
      (message) => message.id && message.status !== 2,
    ).length;
  }

  watch(activeConversation, async () => {
    await autoMarkConversationRead();
  });

  async function autoMarkConversationRead(
    conversationAddress = activeConversation.value,
  ) {
    if (!conversationAddress || !user.value) {
      return;
    }

    const unreadMessages = messages.value.filter((message) => {
      const otherAddress =
        message.from_address === user.value.address
          ? message.to_address
          : message.from_address;
      return (
        otherAddress === conversationAddress &&
        message.status !== 2 &&
        message.to_user_id === user.value.id
      );
    });

    for (const message of unreadMessages) {
      if (!message.id) {
        continue;
      }
      await markMessageRead(message.id);
    }
  }

  // Node discovery management
  function addDiscoveredNode(node) {
    if (!node.address) {
      return;
    }

    const label = node.display_label ??
      buildNodeDisplayLabel({
        name: node.name ?? node.node_name,
        username: node.username,
        ip: node.ip,
        port: node.listen_port ?? node.port,
      });

    const existingIndex = discoveredNodes.value.findIndex(
      (n) => n.address === node.address
    );

    if (existingIndex !== -1) {
      discoveredNodes.value[existingIndex] = {
        ...discoveredNodes.value[existingIndex],
        ...node,
        display_label: label,
      };
    } else {
      discoveredNodes.value.push({
        ...node,
        display_label: label,
      });
    }
  }

  function removeDiscoveredNode(address) {
    discoveredNodes.value = discoveredNodes.value.filter(
      (node) => node.address !== address
    );
  }

  function clearDiscoveredNodes() {
    discoveredNodes.value = [];
  }

  // Contact synchronization
  // Function to synchronize contact status with discovered nodes
  function syncContactStatusWithDiscovery() {
    if (!discoveredNodes.value || !contacts.value) {
      return;
    }

    // Create a map of discovered addresses that are "connected" (online)
    const discoveredOnlineAddresses = new Set();
    discoveredNodes.value.forEach(node => {
      if (node.is_connected) {
        discoveredOnlineAddresses.add(node.address);
      }
    });

    // Update contact status based on discovery list
    contacts.value = contacts.value.map(contact => {
      const isOnline = discoveredOnlineAddresses.has(contact.address);
      return {
        ...contact,
        status: isOnline ? "online" : "offline",
        is_online: isOnline
      };
    });
  }

  // Set up a periodic sync of contact status with discovery status
  const statusSyncInterval = setInterval(() => {
    if (isAuthenticated.value) {
      syncContactStatusWithDiscovery();
    }
  }, 2000); // Sync every 2 seconds

  // Clean up interval when the store is destroyed
  function cleanup() {
    if (statusSyncInterval) {
      clearInterval(statusSyncInterval);
    }
  }

  return {
    // state
    user,
    nodeInfo,
    contacts,
    messages,
    networkStatus,
    peerCount,
    networkRunning,
    unreadCount,
    loading,
    error,
    activeConversation,
    discoveredNodes, // Export the list of discovered nodes

    // getters
    isAuthenticated,
    sortedContacts,
    filteredMessages,

    // actions
    login,
    register,
    logout,
    refreshContacts,
    refreshMessages,
    refreshNodeInfo,
    refreshDiscoveredNodes, // Export the function to refresh discovered nodes
    sendMessage,
    markMessageRead,
    markAllMessagesRead,
    updateContact,
    selectConversation,
    ensureNodeConnection,
    ensureEventListeners,
    teardownEventListeners,
    setError,
    setLoading,
  };
});
