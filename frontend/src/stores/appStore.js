import { defineStore } from "pinia";
import { computed, reactive, ref, watch } from "vue";
import { listen } from "@tauri-apps/api/event";
import { API } from "../services/api";
import { useFeedbackStore } from "./feedbackStore";
import Logger from "../utils/logger";
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
  const fileTransfers = ref({});
  const pendingIncomingTransfers = ref([]);

  // Background interval handles
  let contactRefreshInterval = null;
  let statusSyncInterval = null;

  // Computed properties
  const isAuthenticated = computed(() => Boolean(user.value));

  function normalizeConversationKey(key) {
    if (typeof key !== "string") {
      return null;
    }
    const trimmed = key.trim();
    return trimmed.length ? trimmed : null;
  }

  function conversationKeyForContact(contact) {
    if (!contact || typeof contact !== "object") {
      return null;
    }
    const key =
      contact.user_id ??
      contact.userId ??
      contact.address ??
      contact.public_key ??
      null;
    if (typeof key !== "string") {
      return null;
    }
    return normalizeConversationKey(key);
  }

  const sortedContacts = computed(() => {
    // Log discovery nodes
    console.log("[APP STORE] Discovery nodes for contact matching:", {
      totalDiscovered: discoveredNodes.value.length,
      discoveredNodes: discoveredNodes.value.map(node => ({
        user_id: node.user_id,
        address: node.address,
        ip: node.ip,
        port: node.port,
        is_connected: node.is_connected,
        status: node.status,
        name: node.name,
        username: node.username
      }))
    });

    // Create a map of discovered nodes by user_id for quick lookup
    const discoveredNodeMap = {};
    const discoveredNodeByAddress = {};
    discoveredNodes.value.forEach(node => {
      if (node.user_id) {
        discoveredNodeMap[node.user_id] = node;
      }
      if (node.address) {
        discoveredNodeByAddress[node.address] = node;
      }
    });

    // Create updated contacts list by merging with discovery info
    const updatedContacts = contacts.value.map(contact => {
      // Look for matching discovered node by user_id
      const discoveredNode =
        (contact.user_id && discoveredNodeMap[contact.user_id]) ||
        (contact.address && discoveredNodeByAddress[contact.address]);
      
      // Log the matching process
      if (contact.user_id) {
        console.log(`[APP STORE] Contact matching - Contact user_id: ${contact.user_id}, Found in discovery: ${!!discoveredNode}, Is online: ${discoveredNode?.is_connected || false}`);
      }
      
      if (discoveredNode) {
        // Use discovery node info to update contact status and address
        console.log(`[APP STORE] Updating contact ${contact.name} with discovery info: is_connected=${discoveredNode.is_connected}, ip=${discoveredNode.ip}, address=${discoveredNode.address}`);
        const mergedStatus = discoveredNode.status ?? (discoveredNode.is_connected ? "online" : "offline");
        return {
          ...contact,
          // Priority: Use discovery node status
          status: mergedStatus,
          is_online: Boolean(discoveredNode.is_connected),
          // Priority: Use discovery node address info
          ip: discoveredNode.ip || contact.ip,
          address: discoveredNode.address || contact.address,
          listen_port: discoveredNode.listen_port || contact.listen_port,
          // Preserve name from contact but update display if needed
          display_label: discoveredNode.display_label || contact.display_label,
          // Update other discovery-related fields
          name: discoveredNode.display_label || contact.name,
        };
      }

      // If no matching discovered node, mark contact as offline
      const offlineLabel =
        contact.display_label ||
        buildNodeDisplayLabel({
          name: contact.name,
          username: contact.username,
          ip: contact.ip,
          port: contact.listen_port || contact.port,
        });

      return {
        ...contact,
        is_online: false,
        status: "offline",
        display_label: offlineLabel,
      };
    });

    return updatedContacts.sort((a, b) => a.name.localeCompare(b.name));
  });

  const filteredMessages = computed(() => {
    if (!activeConversation.value) {
      return messages.value;
    }

    return messages.value.filter(
      (message) => messageConversationKey(message) === activeConversation.value,
    );
  });

  const activeContact = computed(() => {
    const activeKey = normalizeConversationKey(activeConversation.value);
    if (!activeKey) {
      return null;
    }

    return (
      contacts.value.find((contact) => {
        const contactKey = normalizeConversationKey(
          conversationKeyForContact(contact),
        );
        return contactKey && contactKey === activeKey;
      }) ?? null
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
    // Use the name field from backend if username is not available
    const username = contact.username ?? contact.name ?? "Unknown"; 
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
      // Preserve the original name field from the backend, don't override with displayLabel
      name: contact.name ?? username, 
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
    let index = current.findIndex(
      (entry) => entry.id === normalized.id && normalized.id !== 0,
    );

    if (index === -1) {
      // If no ID match (or ID is 0), try to find potential duplicate by content, sender, and time
      index = current.findIndex(
        (entry) =>
          entry.content === normalized.content &&
          entry.from_address === normalized.from_address &&
          Math.abs((entry.sent_at || 0) - (normalized.sent_at || 0)) < 2, // Within 2 seconds
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

  function mergeMessageFileState(transferId, next = {}) {
    if (!transferId) {
      return;
    }

    const index = messages.value.findIndex(
      (entry) => entry?.file?.transferId === transferId,
    );

    if (index === -1) {
      return;
    }

    const current = messages.value[index];
    const updated = {
      ...current,
      file: {
        ...(current.file ?? {}),
        ...next,
      },
    };
    messages.value.splice(index, 1, updated);
  }

  function updateTransferState(transferId, payload = {}) {
    if (!transferId) {
      return;
    }
    const existing = fileTransfers.value[transferId] ?? {};
    fileTransfers.value[transferId] = {
      ...existing,
      ...payload,
      transferId,
    };
  }

  function handleFileTransferProgressEvent(payload = {}) {
    const transferId = payload.transferId;
    if (!transferId) {
      return;
    }
    const bytes = Number(payload.bytes ?? 0);
    const total = Number(payload.total ?? 0);
    const direction =
      payload.direction ?? fileTransfers.value[transferId]?.direction ?? "outgoing";

    updateTransferState(transferId, {
      bytesTransferred: bytes,
      totalBytes: total,
      direction,
    });

    mergeMessageFileState(transferId, {
      bytesTransferred: bytes,
      fileSize: total || undefined,
      direction,
    });
  }

  function handleFileTransferStatusEvent(payload = {}) {
    const transferId = payload.transferId;
    if (!transferId) {
      return;
    }
    const status = String(payload.status ?? "pending").toLowerCase();
    const errorMessage = payload.error ?? null;
    const direction =
      payload.direction ?? fileTransfers.value[transferId]?.direction ?? "outgoing";

    updateTransferState(transferId, {
      status,
      error: errorMessage,
      direction,
    });

    mergeMessageFileState(transferId, {
      status,
      error: errorMessage,
      direction,
    });

    if (status === "failed") {
      dequeueIncomingTransfer(transferId);
    }
  }

  function handleFileTransferOfferEvent(payload = {}) {
    const transferId = payload.transferId ?? payload.transfer_id;
    if (!transferId) {
      return;
    }

    const fileName = payload.fileName ?? payload.file_name ?? transferId;
    const fileSize = Number(payload.fileSize ?? payload.file_size ?? 0);
    const senderUserId = payload.senderUserId ?? payload.sender_user_id ?? null;
    const isOutgoing =
      senderUserId && user.value?.user_id
        ? senderUserId === user.value.user_id
        : false;
    const direction = isOutgoing ? "outgoing" : "incoming";
    const requiresSavePath = !isOutgoing && (payload.requestSavePath ?? payload.request_save_path ?? true);

    updateTransferState(transferId, {
      status: "pending",
      direction,
      fileName,
      fileSize,
      checksum: payload.checksum ?? null,
      requiresSavePath,
      senderUserId,
      senderAddress: payload.senderAddress ?? payload.sender_address ?? null,
      senderName: payload.senderName ?? payload.sender_name ?? null,
    });

    mergeMessageFileState(transferId, {
      status: "pending",
      direction,
      fileSize,
    });

    if (requiresSavePath) {
      pendingIncomingTransfers.value.push({
        transferId,
        fileName,
      });
    }
  }

  function handleFileTransferCompleteEvent(payload = {}) {
    const transferId = payload.transferId;
    if (!transferId) {
      return;
    }
    const path = payload.path ?? null;
    const checksumValid = payload.checksumValid ?? true;
    const direction =
      payload.direction ?? fileTransfers.value[transferId]?.direction ?? "outgoing";

    updateTransferState(transferId, {
      status: "completed",
      path,
      checksumValid,
      direction,
      bytesTransferred:
        fileTransfers.value[transferId]?.totalBytes ??
        fileTransfers.value[transferId]?.bytesTransferred ??
        null,
    });

    mergeMessageFileState(transferId, {
      status: "completed",
      checksumValid,
      localPath: path,
      direction,
    });

    dequeueIncomingTransfer(transferId);
  }

  function dequeueIncomingTransfer(transferId) {
    pendingIncomingTransfers.value = pendingIncomingTransfers.value.filter(
      (entry) => entry.transferId !== transferId,
    );
  }

  // Message conversation key function

  // Message conversation key function
  function messageConversationKey(message) {
    if (!message) {
      return null;
    }

    const selfUserId = user.value?.user_id ?? user.value?.id ?? null;
    const selfAddress = user.value?.address ?? null;

    const normalizeKey = (value) => {
      if (!value || typeof value !== "string") {
        return null;
      }
      const trimmed = value.trim();
      return trimmed.length ? trimmed : null;
    };

    if (selfUserId && message.from_user_id === selfUserId) {
      return (
        normalizeKey(message.to_user_id) ||
        normalizeKey(message.to_address) ||
        normalizeKey(message.from_address)
      );
    }

    if (selfUserId && message.to_user_id === selfUserId) {
      return (
        normalizeKey(message.from_user_id) ||
        normalizeKey(message.from_address) ||
        normalizeKey(message.to_address)
      );
    }

    if (selfAddress && message.from_address === selfAddress) {
      return normalizeKey(message.to_address);
    }

    if (selfAddress && message.to_address === selfAddress) {
      return normalizeKey(message.from_address);
    }

    return (
      normalizeKey(message.from_user_id) ||
      normalizeKey(message.from_address) ||
      normalizeKey(message.to_user_id) ||
      normalizeKey(message.to_address)
    );
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

    Logger.error(err);
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
      console.log("[APP STORE] Attempting login for user:", username);
      const result = await API.auth.login(username, password);
      if (!result.success) {
        throw new Error(result.error ?? "Unable to login");
      }
      console.log("[APP STORE] Login successful, user:", result.user);
      cleanup(); // Clean up any existing intervals before setting up new ones
      user.value = result.user;
      await bootstrapAfterAuth();
      feedback.showSuccess(`Signed in as ${result.user?.name ?? username}`, {
        autoDismiss: 3200,
      });
      console.log("[APP STORE] Login process completed successfully");
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
      Object.keys(fileTransfers.value).forEach((key) => {
        delete fileTransfers.value[key];
      });
      pendingIncomingTransfers.value = [];
      setLoading(false);
      feedback.endTask(taskKey);
    }
  }

  // Data refresh actions
  async function bootstrapAfterAuth() {
    console.log("[APP STORE] Starting bootstrap after authentication...");
    await Promise.all([
      refreshContacts(),
      refreshMessages(),
      refreshNodeInfo(),
      refreshDiscoveredNodes(),
    ]);
    await refreshTransfers();
    console.log("[APP STORE] Bootstrap completed, contacts, messages, node info, and discovered nodes updated");
    startBackgroundIntervals();
    ensureEventListeners();
  }

  // Refresh node information from the backend
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
      
      // Log successful refresh
      await Logger.network("node-info-refresh", {
        nodeName: info.name,
        nodePort: info.port,
        nodeStatus: info.status,
        networkStatus: networkStatus.value
      });
    } catch (err) {
      setError(err, {
        message: "Failed to load node information",
        source: "store.refreshNodeInfo",
        toast: false,
      });
      
      // Log error
      await Logger.error("Failed to refresh node info", {
        error: err.message,
        source: "store.refreshNodeInfo"
      });
    }
  }

  async function refreshDiscoveredNodes() {
    // This function is now handled by the nodes-discovered event
    // We keep it for compatibility but it doesn't do anything
    return Promise.resolve();
  }

    // Refresh contacts from the backend
  async function refreshContacts() {
    console.log("[APP STORE] Starting contacts refresh from backend API...");
    try {
      const result = await API.contacts.getContacts();
      if (result.success) {
        const contactsFromApi = Array.isArray(result.contacts) ? result.contacts : [];

        console.log("[APP STORE] Successfully received contacts from backend:", {
          count: contactsFromApi.length,
          contacts: contactsFromApi.map(c => ({
            id: c.id,
            name: c.name,
            user_id: c.user_id, // Added for debugging
            username: c.username,
            address: c.address,
            is_online: c.is_online,
            status: c.status
          }))
        });
        
        applyContacts(contactsFromApi);
        
        // Log successful refresh and print contact data
        console.log("[APP STORE] Contacts applied to store:", {
          contactCount: contactsFromApi.length,
          contacts: contactsFromApi.map(c => ({
            id: c.id,
            name: c.name,
            username: c.username,
            address: c.address,
            is_online: c.is_online,
            status: c.status
          }))
        });
        
        await Logger.contact("contacts-refresh", {
          contactCount: contactsFromApi.length
        });
      } else {
        console.error("[APP STORE] Failed to get contacts from backend:", result);
      }
    } catch (err) {
      console.error("[APP STORE] Error refreshing contacts from backend:", err);
      setError(err, {
        message: "Failed to refresh contacts",
        source: "store.refreshContacts",
        toast: false,
      });
      
      // Log error
      await Logger.error("Failed to refresh contacts", {
        error: err.message,
        source: "store.refreshContacts"
      });
    }
  }

  // Refresh messages from the backend
  async function refreshMessages() {
    try {
      const result = await API.messages.getMessages();
      const normalized = normalizeMessageList(Array.isArray(result) ? result : []);
      messages.value = normalized;
      recomputeUnread();
      
      // Log successful refresh
      await Logger.chat("messages-refresh", {
        messageCount: normalized.length,
        unreadCount: unreadCount.value
      });
    } catch (err) {
      setError(err, {
        message: "Failed to refresh messages",
        source: "store.refreshMessages",
        toast: false,
      });
      
      // Log error
      await Logger.error("Failed to refresh messages", {
        error: err.message,
        source: "store.refreshMessages"
      });
    }
  }

  // Send a message to connected peers
  async function sendMessage(content, targetContact = null) {
    const trimmed = content.trim();
    if (trimmed.length === 0) {
      const error = new Error("Message content cannot be empty");
      setError(error, {
        message: "Message content cannot be empty",
        source: "messages.send",
        toast: true,
      });
      
      // Log validation error
      await Logger.error("Empty message content", {
        error: "Message content cannot be empty",
        source: "messages.send"
      });
      
      return { success: false, error: "Message content cannot be empty" };
    }

    try {
      const target = targetContact || activeContact.value || null;
      const targetUserId = target?.user_id ?? target?.userId ?? null;
      const fallbackAddress =
        typeof activeConversation.value === "string" &&
        activeConversation.value.includes(":")
          ? activeConversation.value
          : null;
      const targetAddress =
        target?.address ?? target?.public_key ?? fallbackAddress;

      const result = await API.messages.sendMessage(trimmed, {
        userId: targetUserId,
        address: targetAddress,
      });
      if (result?.success || (result && typeof result === "object" && result.id)) {
        const messagePayload = result?.message ?? result;
        const message = upsertMessage(messagePayload);
        if (!message) {
          return { success: true, message: messagePayload };
        }
        const desiredKey =
          conversationKeyForContact(target) ||
          messageConversationKey(message) ||
          activeConversation.value;
        if (desiredKey && desiredKey !== activeConversation.value) {
          activeConversation.value = desiredKey;
        }

        // Log successful send
        await Logger.chat("message-sent", {
          messageId: message.id,
          contentLength: trimmed.length,
          recipient: activeConversation.value
        });
        
        return { success: true, message };
      } else {
        const error = new Error(result?.message ?? "Failed to send message");
        setError(error, {
          message: result?.message ?? "Failed to send message",
          source: "messages.send",
          toast: true,
        });
        
        // Log send failure
        await Logger.error("Failed to send message", {
          error: result?.message ?? "Failed to send message",
          source: "messages.send"
        });
        
        return { success: false, error: result?.message ?? "Failed to send message" };
      }
    } catch (err) {
      setError(err, {
        message: "Failed to send message",
        source: "messages.send",
        toast: true,
      });
      
      // Log exception
      await Logger.error("Exception while sending message", {
        error: err.message,
        source: "messages.send",
        stack: err.stack
      });
      
      return { success: false, error: err.message };
    }
  }

  async function sendFile(filePath, targetContact = null) {
    if (!filePath || typeof filePath !== "string") {
      return {
        success: false,
        error: "请选择要发送的文件",
      };
    }

    const target = targetContact || activeContact.value || null;
    const conversationKey =
      conversationKeyForContact(target) ?? activeConversation.value;
    const targetUserId = target?.user_id ?? target?.userId ?? null;
    const addressFromTarget = target?.address ?? target?.public_key ?? null;
    const targetAddress =
      addressFromTarget ||
      (typeof conversationKey === "string" && conversationKey.includes(":")
        ? conversationKey
        : null);

    if (!targetUserId && !targetAddress) {
      const error = "请选择一个联系人后再发送文件";
      setError(new Error(error), {
        message: error,
        source: "files.send",
        toast: true,
      });
      return {
        success: false,
        error,
      };
    }

    try {
      setLoading(true);
      const connectionKey = target || conversationKey || targetAddress;
      if (connectionKey) {
        await ensureNodeConnection(connectionKey);
      }

      const result = await API.files.sendFile(filePath, {
        userId: targetUserId,
        address: targetAddress,
      });

      const transferId = result?.transferId ?? result?.transfer_id ?? null;
      if (transferId) {
        updateTransferState(transferId, {
          status: "pending",
          direction: "outgoing",
          bytesTransferred: 0,
          totalBytes: result?.message?.fileSize ?? 0,
          localPath: filePath,
        });
      }

      if (result?.message) {
        const message = upsertMessage(result.message);
        if (message?.file?.transferId) {
          mergeMessageFileState(message.file.transferId, {
            status: "pending",
            bytesTransferred: 0,
            fileSize: message.file.fileSize ?? 0,
            localPath: filePath,
            direction: "outgoing",
          });
        }
      }

      if (conversationKey && conversationKey !== activeConversation.value) {
        activeConversation.value = conversationKey;
      }

      return {
        success: true,
        transferId,
      };
    } catch (err) {
      console.error("[APP STORE] Failed to send file", err);
      setError(err, {
        message: err?.message ?? "发送文件失败",
        source: "files.send",
        toast: true,
      });
      await Logger.error("file-send-failed", {
        error: err?.message ?? err,
        filePath,
      });
      return {
        success: false,
        error: err?.message ?? "发送文件失败",
      };
    } finally {
      setLoading(false);
    }
  }

  async function acceptIncomingFileTransfer(transferId, savePath) {
    if (!transferId) {
      return;
    }
    try {
      await API.files.acceptIncoming(transferId, savePath ?? null);
      dequeueIncomingTransfer(transferId);
    } catch (err) {
      setError(err, {
        message: err?.message ?? "无法开始接收文件",
        source: "files.accept",
        toast: true,
      });
      await Logger.error("file-accept-failed", {
        transferId,
        error: err?.message ?? err,
      });
    }
  }

  async function rejectIncomingFileTransfer(transferId) {
    if (!transferId) {
      return;
    }
    try {
      await API.files.rejectIncoming(transferId);
    } catch (err) {
      setError(err, {
        message: err?.message ?? "无法取消接收",
        source: "files.reject",
        toast: true,
      });
      await Logger.error("file-reject-failed", {
        transferId,
        error: err?.message ?? err,
      });
    } finally {
      dequeueIncomingTransfer(transferId);
    }
  }

  async function resumeFileTransfer(transferId) {
    if (!transferId) {
      return;
    }
    try {
      await API.files.resumeTransfer(transferId);
      updateTransferState(transferId, {
        status: "in_progress",
      });
      mergeMessageFileState(transferId, {
        status: "in_progress",
      });
    } catch (err) {
      setError(err, {
        message: err?.message ?? "无法继续传输",
        source: "files.resume",
        toast: true,
      });
      await Logger.error("file-resume-failed", {
        transferId,
        error: err?.message ?? err,
      });
    }
  }

  async function cancelFileTransfer(transferId) {
    if (!transferId) {
      return;
    }
    try {
      await API.files.cancelTransfer(transferId);
      updateTransferState(transferId, {
        status: "paused",
      });
      mergeMessageFileState(transferId, {
        status: "paused",
      });
    } catch (err) {
      setError(err, {
        message: err?.message ?? "无法暂停传输",
        source: "files.cancel",
        toast: true,
      });
      await Logger.error("file-cancel-failed", {
        transferId,
        error: err?.message ?? err,
      });
    }
  }

  async function refreshTransfers() {
    try {
      const result = await API.files.listTransfers();
      if (!result?.transfers) {
        return;
      }

      Object.keys(fileTransfers.value).forEach((key) => {
        delete fileTransfers.value[key];
      });

      for (const manifest of result.transfers) {
        const transferId = manifest.transfer_id ?? manifest.transferId;
        if (!transferId) {
          continue;
        }
        const status = String(manifest.status ?? "pending").toLowerCase();
        const direction =
          typeof manifest.direction === "string"
            ? manifest.direction.toLowerCase()
            : "outgoing";
        updateTransferState(transferId, {
          status,
          direction,
          bytesTransferred: manifest.bytes_confirmed ?? 0,
          totalBytes: manifest.file_size ?? 0,
          localPath: manifest.local_path ?? null,
          tempPath: manifest.temp_path ?? null,
          checksum: manifest.checksum ?? null,
          fileName: manifest.file_name ?? transferId,
          requiresSavePath: status === "pending" && direction === "incoming",
        });

        mergeMessageFileState(transferId, {
          status,
          bytesTransferred: manifest.bytes_confirmed ?? 0,
          fileSize: manifest.file_size ?? 0,
          localPath: manifest.local_path ?? null,
          direction,
        });

        if (status === "pending" && direction === "incoming") {
          const alreadyQueued = pendingIncomingTransfers.value.some(
            (entry) => entry.transferId === transferId,
          );
          if (!alreadyQueued) {
            pendingIncomingTransfers.value.push({
              transferId,
              fileName: manifest.file_name ?? transferId,
            });
          }
        }
      }
    } catch (err) {
      setError(err, {
        message: err?.message ?? "无法同步传输状态",
        source: "files.refresh",
        toast: false,
      });
      await Logger.error("file-transfers-refresh-failed", {
        error: err?.message ?? err,
      });
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

  // Contact management actions
  async function updateContact(contactId, data) {
    try {
      const result = await API.contacts.updateContact(contactId, data);

      // Update the contact in the local store if successful
      if (result.success && result.contact) {
        const contactIndex = contacts.value.findIndex(
          (contact) => contact.id === contactId,
        );
        if (contactIndex !== -1) {
          // Preserve the original name if it was explicitly provided in the update
          const updatedContact = {
            ...contacts.value[contactIndex],
            ...result.contact,
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
  function selectConversation(target) {
    if (!target) {
      activeConversation.value = null;
      return;
    }

    if (typeof target === "string") {
      const trimmed = target.trim();
      activeConversation.value = trimmed.length ? trimmed : null;
      return;
    }

    const key =
      conversationKeyForContact(target) ??
      target.address ??
      target.public_key ??
      null;
    activeConversation.value = key;
  }

  async function ensureNodeConnection(target) {
    const contact = target && typeof target === "object" ? target : null;
    const userId = contact?.user_id ?? contact?.userId ?? null;
    const conversationKey =
      typeof target === "string"
        ? target.trim() || null
        : conversationKeyForContact(contact);
    const address =
      typeof target === "string"
        ? target.trim() || null
        : contact?.address ?? contact?.public_key ?? null;

    const label =
      contact?.display_label ??
      contact?.name ??
      address ??
      userId ??
      "unknown";

    try {
      if (userId) {
        console.log(
          `[APP STORE] Ensuring TCP channel for contact ${label} (user_id=${userId})`,
        );
        const result = await API.node.connectToUser(userId);
        if (result?.success === false) {
          throw new Error(result?.message || "Unable to establish TCP channel");
        }
        console.log(
          `[APP STORE] TCP channel ready for ${label} via user_id`,
          result,
        );
        await Logger.network("tcp-connect-user", {
          label,
          userId,
          address: result?.address ?? address ?? null,
          reused: Boolean(result?.reused),
        });
        activeConversation.value = conversationKey ?? result?.address ?? address ?? userId;
        return;
      }

      const trimmedAddress = address?.trim();
      if (trimmedAddress) {
        console.log(
          `[APP STORE] Ensuring TCP channel for contact ${label} via address ${trimmedAddress}`,
        );
        await API.node.connectToNode(trimmedAddress);
        await Logger.network("tcp-connect-address", {
          label,
          address: trimmedAddress,
          reused: false,
        });
        activeConversation.value = conversationKey ?? trimmedAddress;
      } else {
        console.warn(
          "[APP STORE] Unable to ensure TCP connection: no user_id or address provided",
          {
            target,
            conversationKey,
          },
        );
      }
    } catch (err) {
      console.error(
        `[APP STORE] Failed to ensure TCP connection for ${label}:`,
        err,
      );
      setError(err, {
        message: "Failed to connect to node",
        source: "network.connect",
        toast: true,
      });
      await Logger.error("tcp-connect-failed", {
        label,
        userId,
        address,
        error: err?.message ?? err,
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
          const normalized =
            upsertMessage(incomingMessage) ?? normalizeMessage(incomingMessage);

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
        const updatedUsername = current.username ?? user.value?.name ?? "Guest";

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
        console.log("[FRONTEND] Received nodes-discovered event:", event);
        const payload = event.payload ?? {};
        console.log("[FRONTEND] Payload nodes:", payload.nodes);
        if (payload.nodes) {
          console.log("[FRONTEND] Processing", payload.nodes.length, "nodes:");
          discoveredNodes.value = payload.nodes.map((node) => {
            console.log("[FRONTEND] Node:", {
              name: node.name,
              username: node.username,
              ip: node.ip,
              user_id: node.user_id,
              source_ip: node.source_ip
            });
            
            return {
              ...node,
              display_label:
                node.display_label ??
                buildNodeDisplayLabel({
                  name: node.name,
                  username: node.username ?? "Unknown",
                  ip: node.ip ?? splitAddress(node.address)[0],
                  port: node.listen_port ?? node.port ?? undefined,
                }),
            };
          });
          console.log("[FRONTEND] Updated discoveredNodes:", discoveredNodes.value);
        }
      }),
      listen("file-transfer-progress", (event) => {
        handleFileTransferProgressEvent(event.payload ?? {});
      }),
      listen("file-transfer-status", (event) => {
        handleFileTransferStatusEvent(event.payload ?? {});
      }),
      listen("file-transfer-complete", (event) => {
        handleFileTransferCompleteEvent(event.payload ?? {});
      }),
      listen("file-transfer-offer", (event) => {
        handleFileTransferOfferEvent(event.payload ?? {});
      }),
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
        (contact) => contact.address === payload.address,
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
    conversationKey = activeConversation.value,
  ) {
    if (!conversationKey || !user.value) {
      return;
    }

    const selfUserId = user.value.user_id;
    const normalizedKey =
      typeof conversationKey === "string" ? conversationKey.trim() : null;

    const unreadMessages = messages.value.filter((message) => {
      if (!message || message.status === 2) {
        return false;
      }

      const otherKey = (function () {
        if (message.from_user_id === selfUserId) {
          return message.to_user_id ?? message.to_address ?? null;
        }
        if (message.to_user_id === selfUserId) {
          return message.from_user_id ?? message.from_address ?? null;
        }
        if (message.from_address === user.value.address) {
          return message.to_address ?? null;
        }
        if (message.to_address === user.value.address) {
          return message.from_address ?? null;
        }
        return message.from_user_id ?? message.from_address ?? null;
      })();

      const normalizedOtherKey =
        typeof otherKey === "string" ? otherKey.trim() : null;

      return (
        normalizedOtherKey &&
        normalizedKey &&
        normalizedOtherKey === normalizedKey &&
        message.to_user_id === selfUserId
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

    const label =
      node.display_label ??
      buildNodeDisplayLabel({
        name: node.name ?? node.node_name,
        username: node.username,
        ip: node.ip,
        port: node.listen_port ?? node.port,
      });

    const existingIndex = discoveredNodes.value.findIndex(
      (n) => n.address === node.address,
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
      (node) => node.address !== address,
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

    // Log discovery nodes for sync
    console.log("[APP STORE] Syncing contact status with discovery nodes:", {
      totalDiscovered: discoveredNodes.value.length,
      totalContacts: contacts.value.length,
      discoveredNodes: discoveredNodes.value.map(node => ({
        user_id: node.user_id,
        address: node.address,
        ip: node.ip,
        port: node.port,
        is_connected: node.is_connected,
        status: node.status
      }))
    });

    // Create a map of discovered nodes by user_id for quick lookup
    const discoveredNodeMap = {};
    const discoveredNodeByAddress = {};
    discoveredNodes.value.forEach(node => {
      if (node.user_id) {
        discoveredNodeMap[node.user_id] = node;
      }
      if (node.address) {
        discoveredNodeByAddress[node.address] = node;
      }
    });

    // Update contact status and IP/port info based on discovery list using user_id matching
    contacts.value = contacts.value.map((contact) => {
      // Look for matching discovered node by user_id
      const discoveredNode =
        (contact.user_id && discoveredNodeMap[contact.user_id]) ||
        (contact.address && discoveredNodeByAddress[contact.address]);

      // Log the matching process
      if (contact.user_id) {
        console.log(`[APP STORE] Sync - Contact user_id: ${contact.user_id}, Found in discovery: ${!!discoveredNode}, Is online: ${discoveredNode?.is_connected || false}`);
      }

      if (discoveredNode) {
        // Use discovery node info to update contact
        console.log(`[APP STORE] Sync - Updating contact ${contact.name} with discovery info: is_connected=${discoveredNode.is_connected}, ip=${discoveredNode.ip}, address=${discoveredNode.address}`);
        const mergedStatus = discoveredNode.status ?? (discoveredNode.is_connected ? "online" : "offline");
        return {
          ...contact,
          // Priority: Use discovery node status
          status: mergedStatus,
          is_online: Boolean(discoveredNode.is_connected),
          // Priority: Use discovery node address info
          ip: discoveredNode.ip || contact.ip,
          address: discoveredNode.address || contact.address,
          listen_port: discoveredNode.listen_port || contact.listen_port,
          // Update display label from discovery if available
          display_label: discoveredNode.display_label || contact.display_label,
        };
      } else {
        const offlineLabel =
          contact.display_label ||
          buildNodeDisplayLabel({
            name: contact.name,
            username: contact.username,
            ip: contact.ip,
            port: contact.listen_port || contact.port,
          });

        return {
          ...contact,
          is_online: false,
          status: "offline",
          display_label: offlineLabel,
        };
      }
    });
  }

  // Lazily start background intervals once authentication completes
  function startBackgroundIntervals() {
    if (!contactRefreshInterval) {
      console.log("[APP STORE] Starting periodic contact refresh interval");
      contactRefreshInterval = setInterval(async () => {
        if (!isAuthenticated.value) {
          return;
        }
        console.log("[APP STORE] Periodically refreshing contacts from backend...");
        await refreshContacts();
      }, 10000); // Refresh contacts from backend every 10 seconds
    }

    if (!statusSyncInterval) {
      console.log("[APP STORE] Starting contact status sync interval");
      statusSyncInterval = setInterval(() => {
        if (!isAuthenticated.value) {
          return;
        }
        syncContactStatusWithDiscovery();
      }, 2000); // Sync every 2 seconds
    }
  }

  // Clean up intervals when the store is destroyed
  function cleanup() {
    if (contactRefreshInterval) {
      clearInterval(contactRefreshInterval);
      contactRefreshInterval = null;
    }
    if (statusSyncInterval) {
      clearInterval(statusSyncInterval);
      statusSyncInterval = null;
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
    fileTransfers,
    pendingIncomingTransfers,

    // getters
    isAuthenticated,
    sortedContacts,
    activeContact,
    filteredMessages,

    // actions
    login,
    register,
    logout,
    refreshContacts,
    refreshMessages,
    refreshNodeInfo,
    refreshDiscoveredNodes, // Export the function to refresh discovered nodes
    refreshTransfers,
    sendMessage,
    sendFile,
    acceptIncomingFileTransfer,
    rejectIncomingFileTransfer,
    markMessageRead,
    resumeFileTransfer,
    cancelFileTransfer,
    updateContact,
    selectConversation,
    ensureNodeConnection,
    ensureEventListeners,
    teardownEventListeners,
    setError,
    setLoading,
    conversationKeyForContact,
  };
});
