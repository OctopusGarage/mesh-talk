<template>
  <main class="chat-layout">
    <section class="chat-area">
      <div class="chat-window-wrapper">
        <ChatWindow
          :messages="filteredMessages"
          :current-user="user"
          :active-conversation="activeConversation"
          :active-contact="activeContact"
          @mark-read="handleMarkRead"
          @mark-all-read="handleMarkAllRead"
        >
          <template #input>
            <MessageInput :disabled="loading" @send="handleSend" />
          </template>
        </ChatWindow>
      </div>
    </section>

    <aside class="sidebar">
      <section class="panel contacts-panel">
        <div class="panel-header">
          <h3>Contacts</h3>
          <span class="panel-meta">{{ sortedContacts.length }}</span>
        </div>
        <div class="panel-body contacts-body">
          <div v-if="!sortedContacts.length" class="contacts-empty">
            No contacts yet
          </div>
          <ul v-else class="contacts-items">
            <li
              v-for="contact in sortedContacts"
              :key="contact.id ?? contact.address"
              :class="{ active: contact.address === activeConversation }"
              @click="handleSelectContact(contact.address)"
            >
              <div class="contact-info">
                <div class="contact-details">
                  <span class="contact-name">{{ contact.username}}</span>
                  <span class="contact-meta">{{ contact.ip}}</span>
                </div>
                <span class="contact-status-icon" :class="contact.status || 'offline'"></span>
              </div>
            </li>
          </ul>
        </div>
      </section>

      <section class="panel discovery-panel">
        <div class="panel-header">
          <h3>Discovery</h3>
          <span class="panel-meta">{{ discoveredNodeList.length }}</span>
        </div>
        <div class="panel-body discovery-body">
          <div v-if="!discoveredNodeList.length" class="discovery-empty">
            No nodes discovered
          </div>
          <ul v-else class="discovery-items">
            <li
              v-for="node in discoveredNodeList"
              :key="node.unique_key"
              :class="{
                active: node.address === activeConversation,
              }"
            >
              <div class="discovery-info">
                <span
                  class="discovery-name"
                  @mouseenter="hoveredDiscovery = node.address"
                  @mouseleave="hoveredDiscovery = null"
                >
                  {{ node.username }}
                </span>
                <span class="discovery-meta">{{ node.ip }}</span>
              </div>
              <div class="discovery-actions">
                <button
                  class="invite-btn"
                  type="button"
                  @click="handleDiscoveryInvite(node)"
                >
                  Invite
                </button>
              </div>
              <div
                v-if="hoveredDiscovery === node.address"
                class="discovery-overlay"
              >
                <p class="overlay-title">Details</p>
                <ul>
                  <li v-if="node.name"><span>{{ node.name }}</span></li>
                  <li v-if="node.address"><span>{{ node.address }}</span></li>
                </ul>
              </div>
            </li>
          </ul>
        </div>
      </section>

      <section class="panel node-panel">
        <div class="panel-header">
          <h3>Node</h3>
          <span class="panel-meta">{{ networkStatusLabel }}</span>
        </div>
        <div class="panel-body node-body">
          <p
            class="node-label"
            @mouseenter="nodeOverlayActive = true"
            @mouseleave="nodeOverlayActive = false"
          >
            {{ nodePanelInfo.label }}
          </p>
          <ul class="node-stats">
            <li>Peers: <span>{{ peerCount }}</span></li>
            <li>Unread: <span>{{ unreadCount }}</span></li>
          </ul>
          <div class="node-meta-footer">
            <button class="logout-button" @click="logout">Logout</button>
          </div>
          <div v-if="nodeOverlayVisible" class="node-info-overlay">
            <p class="overlay-title">Details</p>
            <ul>
              <li v-if="nodePanelInfo.nodeName"><span>{{ nodePanelInfo.nodeName }}</span></li>
              <li v-if="nodePanelInfo.address"><span>{{ nodePanelInfo.address }}</span></li>
            </ul>
          </div>
        </div>
      </section>
    </aside>
  </main>
  
  <!-- Contact Request Popup -->
  <div v-if="showContactRequestPopup" class="contact-request-popup">
    <div class="popup-content">
      <h3>Contact Request</h3>
      <p>{{ pendingContactRequest?.requester_alias || 'Unknown' }} wants to connect with you.</p>
      <div class="popup-actions">
        <button @click="acceptContactRequest" class="accept-btn">Accept</button>
        <button @click="declineContactRequest" class="decline-btn">Decline</button>
      </div>
    </div>
  </div>
</template>

<script setup>
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useRouter } from "vue-router";
import ContactList from "../../components/contacts/ContactList.vue";
import ChatWindow from "../../components/chat/ChatWindow.vue";
import MessageInput from "../../components/chat/MessageInput.vue";
import { useAppStore } from "../../stores/appStore";
import { storeToRefs } from "pinia";
import { useRealTimeMessages } from "../../composables/chat/useRealTimeMessages";
import { useFeedbackStore } from "../../stores/feedbackStore";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const router = useRouter();
const store = useAppStore();
const feedback = useFeedbackStore();
const { startMessageListener, stopMessageListener } = useRealTimeMessages({
  autoStart: false,
});
const {
  user,
  sortedContacts,
  filteredMessages,
  networkStatus,
  peerCount,
  unreadCount,
  activeConversation,
  nodeInfo,
  loading,
  discoveredNodes, 
} = storeToRefs(store);

const contactRequestUnlisten = ref(null);
const contactAddedUnlisten = ref(null);

const networkStatusLabel = computed(() => {
  if (networkStatus.value === "connected") return "Online";
  if (networkStatus.value === "connecting") return "Connecting";
  return "Offline";
});

const activeContact = computed(() => {
  if (!activeConversation.value) return null;
  
  // Find the contact that matches the active conversation address
  return sortedContacts.value.find(contact => 
    contact.address === activeConversation.value
  ) || null;
});

const nodePanelInfo = computed(() => {
  const info = nodeInfo.value;

  if (!info) {
    return {
      label: user.value?.name ?? "Current Node",
      nodeName: null,
      address: null,
    };
  }

  const nodeName = info.node_name ?? info.name ?? null;
  const username = info.username ?? user.value?.name ?? null;
  const ip = info.ip ?? null;
  const port = info.port ?? null;
  const directAddress = typeof info.address === "string" ? info.address : null;
  const address = directAddress ?? (ip ? `${ip}${port ? `:${port}` : ""}` : null);
  const label = username ?? nodeName ?? address ?? "Current Node";

  return { label, nodeName, address };
});

const nodeOverlayActive = ref(false);
const nodeOverlayAvailable = computed(() => {
  const details = nodePanelInfo.value;
  return Boolean(details.nodeName || details.address || networkStatusLabel.value);
});

const nodeOverlayVisible = computed(() => nodeOverlayActive.value && nodeOverlayAvailable.value);

const hoveredDiscovery = ref(null);

function buildDiscoveredLabel(node) {
  const nodeName = node.name ?? node.node_name ?? "Unknown";
  const username = node.username ?? "Unknown";
  const [ipPart, portPart] = (() => {
    if (node.ip) {
      return [node.ip, node.listen_port ?? node.port ?? undefined];
    }
    if (node.address) {
      const parts = String(node.address).split(":");
      if (parts.length >= 2) {
        const port = Number(parts.pop());
        return [parts.join(":"), Number.isNaN(port) ? undefined : port];
      }
      return [node.address, undefined];
    }
    return ["127.0.0.1", undefined];
  })();
  const port = node.listen_port ?? node.port ?? portPart ?? 0;
  return `${nodeName} • ${username} • ${ipPart}:${port}`;
}

const discoveredNodeList = computed(() => {
  const uniqueAddresses = new Set();
  return discoveredNodes.value
    .filter((node) => {
      if (!node?.address) {
        return false;
      }
      const normalizedAddress = String(node.address).trim();
      if (!normalizedAddress) {
        return false;
      }

      const [ipPart, portPart] = (() => {
        const parts = normalizedAddress.split(':');
        if (parts.length < 2) {
          return [normalizedAddress, undefined];
        }
        const portString = parts.pop() ?? '';
        const port = Number.parseInt(portString, 10);
        return [parts.join(':'), Number.isNaN(port) ? undefined : port];
      })();

      const ip = node.ip ?? ipPart;
      const port = node.listen_port ?? node.port ?? portPart;

      if (!ip || port === undefined) {
        return false;
      }

      const uniqueKey = `${ip}:${port}`;

      if (uniqueAddresses.has(uniqueKey)) {
        return false;
      }
      uniqueAddresses.add(uniqueKey);
      return true;
    })
    .map((node) => {
      const displayLabel = node.display_label ?? buildDiscoveredLabel(node);
      const uniqueKey = (() => {
        const parts = String(node.address).split(':');
        if (parts.length >= 2) {
          const port = parts.pop();
          return `${parts.join(':')}:${port}`;
        }
        return node.address;
      })();
      return {
        ...node,
        display_label: displayLabel,
        status: node.is_connected ? "online" : "offline",
        unique_key: uniqueKey,
      };
    });
});

const handleDiscoveryConnect = async (node) => {
  if (!node || node.isSelf || !node.address) {
    return;
  }

  await store.ensureNodeConnection(node.address);
  store.selectConversation(node.address);
};
const handleSend = async (content) => {
  const result = await store.sendMessage(content);
  if (!result.success && store.error) {
    console.error(store.error);
  }
};

const handleMarkRead = async (messageId) => {
  await store.markMessageRead(messageId);
};

const handleMarkAllRead = async () => {
  await store.markAllMessagesRead();
};

// State for contact request popup
const showContactRequestPopup = ref(false);
const pendingContactRequest = ref(null);

const handleSelectContact = async (address) => {
  await store.ensureNodeConnection(address);
  store.selectConversation(address);
};

const formatNodeAddress = (address) => {
  if (!address) return '';
  
  // Extract just the IP part or hostname part for display
  try {
    // If it's in the format ip:port, just show the IP part
    const parts = address.split(':');
    if (parts.length >= 2) {
      return parts[0];
    }
    return address;
  } catch (e) {
    return address;
  }
};

// Handle incoming contact request
const handleContactRequest = async (event) => {
  console.log('Received contact request:', event.payload);
  pendingContactRequest.value = event.payload;
  showContactRequestPopup.value = true;
};

const resolvePendingRequestJson = () => {
  const payload = pendingContactRequest.value;
  if (!payload) {
    return null;
  }

  const normalize = (rawValue) => {
    if (!rawValue) {
      return null;
    }

    let raw = String(rawValue).trim();
    if (!raw) {
      return null;
    }

    // Attempt to normalise enum-style payloads to the bare ContactRequest struct
    try {
      const parsed = JSON.parse(raw);
      if (parsed && typeof parsed === 'object') {
        if (parsed.requester_public_key && parsed.signature) {
          return JSON.stringify(parsed);
        }

        const variant =
          parsed.ContactRequest || parsed.contact_request || parsed.request;
        if (variant && typeof variant === 'object') {
          return JSON.stringify(variant);
        }
      }
    } catch (err) {
      // Not JSON, fall through and return the raw value
    }

    return raw;
  };

  if (typeof payload === 'string') {
    return normalize(payload);
  }

  if (typeof payload === 'object' && payload !== null) {
    const direct =
      payload.request_json ||
      payload.requestJson ||
      payload.request?.request_json ||
      payload.request?.requestJson;

    if (direct) {
      const normalised = normalize(direct);
      if (normalised) {
        return normalised;
      }
    }

    const base64 = payload.request_json_base64 || payload.requestJsonBase64;
    if (base64) {
      try {
        return normalize(atob(base64));
      } catch (err) {
        console.error('Failed to decode base64 contact request:', err);
      }
    }
  }

  return null;
};

  // Handle discovery invite
const handleDiscoveryInvite = async (node) => {
  console.log('Handling discovery invite for node:', node);
  if (!node || !node.address) {
    console.error('Invalid node for invitation');
    return;
  }

  // Extract the public key from the node's address or other properties
  // In a real implementation, we would have the public key available in the node object
  const targetPublicKey = node.public_key || node.address; // Fallback to address if no public key
  
  console.log('Sending contact request to public key:', targetPublicKey);
  
  try {
    const result = await invoke('send_contact_request', {
      targetPublicKey: targetPublicKey,
      alias: node.display_label || node.name
    });
    
    console.log('Contact request result:', result);
    
    if (result.success) {
      feedback.showSuccess(`Invitation sent to ${node.display_label || node.name}`);
    } else {
      feedback.showError(`Failed to send invitation: ${result.message || 'Unknown error'}`);
    }
  } catch (error) {
    console.error('Error sending contact request:', error);
    feedback.showError(`Error sending invitation: ${error.message}`);
  }
};

// Accept contact request
const acceptContactRequest = async () => {
  const requestJson = resolvePendingRequestJson();
  if (!requestJson) {
    feedback.showError('Unable to parse contact request payload.');
    showContactRequestPopup.value = false;
    pendingContactRequest.value = null;
    return;
  }

  try {
    const result = await invoke('handle_contact_request', {
      requestJson,
      approve: true
    });
    
    if (result.success) {
      feedback.showSuccess(`Contact request from ${pendingContactRequest.value.requester_alias} accepted`);
      // Refresh contacts to show the new contact
      await store.refreshContacts();
    } else {
      feedback.showError(`Failed to accept contact request: ${result.message || 'Unknown error'}`);
    }
  } catch (error) {
    console.error('Error accepting contact request:', error);
    feedback.showError(`Error accepting contact request: ${error.message}`);
  } finally {
    showContactRequestPopup.value = false;
    pendingContactRequest.value = null;
  }
};

// Decline contact request
const declineContactRequest = async () => {
  const requestJson = resolvePendingRequestJson();
  if (!requestJson) {
    feedback.showError('Unable to parse contact request payload.');
    showContactRequestPopup.value = false;
    pendingContactRequest.value = null;
    return;
  }

  try {
    const result = await invoke('handle_contact_request', {
      requestJson,
      approve: false
    });
    
    if (result.success) {
      feedback.showInfo(`Contact request from ${pendingContactRequest.value.requester_alias} declined`);
    } else {
      feedback.showError(`Failed to decline contact request: ${result.message || 'Unknown error'}`);
    }
  } catch (error) {
    console.error('Error declining contact request:', error);
    feedback.showError(`Error declining contact request: ${error.message}`);
  } finally {
    showContactRequestPopup.value = false;
    pendingContactRequest.value = null;
  }
};

const logout = async () => {
  await store.logout();
  router.push({ name: "login" });
};

onMounted(async () => {
  if (!store.isAuthenticated) {
    router.replace({ name: "login" });
    return;
  }
  await startMessageListener();
  
  // Initial data loading
  await Promise.all([
    store.refreshContacts(),
    store.refreshMessages(),
    store.refreshNodeInfo(),
  ]);
  
  // Listen for contact request events
  contactRequestUnlisten.value = await listen('contact-request-received', handleContactRequest);
  contactAddedUnlisten.value = await listen('contact-added', async () => {
    await store.refreshContacts();
  });
});

onBeforeUnmount(() => {
  void stopMessageListener();
  
  // Clean up contact request event listener
  if (contactRequestUnlisten.value) {
    contactRequestUnlisten.value();
  }
  if (contactAddedUnlisten.value) {
    contactAddedUnlisten.value();
  }
});
</script>

<style scoped>
.chat-layout {
  min-height: 100vh;
  min-width: calc(250px + 80px + 3rem);
  display: grid;
  grid-template-columns: minmax(250px, 3fr) minmax(80px, 1fr);
  gap: 1.5rem;
  padding: 1.5rem;
  align-items: stretch;
  background: radial-gradient(circle at top left, rgba(30, 64, 175, 0.25), transparent 55%),
    radial-gradient(circle at bottom right, rgba(14, 116, 144, 0.2), transparent 50%);
  width: 100%;
  max-width: 100vw;
  height: 100vh;
  box-sizing: border-box;
}

.chat-area {
  display: flex;
  flex-direction: column;
  min-height: 0;
  overflow: hidden;
  min-width: 250px;
  flex: none;
}

.chat-window-wrapper {
  flex: 1;
  min-height: 0;
  min-width: 0;
  display: flex;
  border-radius: 16px;
  background: rgba(15, 23, 42, 0.65);
  border: 1px solid rgba(148, 163, 184, 0.15);
  box-shadow: 0 10px 25px rgba(15, 23, 42, 0.35);
  overflow: hidden;
}

.chat-window-wrapper :deep(> *) {
  flex: 1;
}

.sidebar {
  display: grid;
  grid-template-rows: repeat(3, minmax(160px, 1fr));
  gap: 1rem;
  overflow-y: auto;
  min-width: 80px;
}

.panel {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  padding: 0.75rem;
  border-radius: 16px;
  background: rgba(15, 23, 42, 0.7);
  border: 1px solid rgba(148, 163, 184, 0.12);
  box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.04);
  min-height: 160px;
  min-width: 0;
}

.panel-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 0.75rem;
}

.panel-header h3 {
  margin: 0;
  font-size: 0.85rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: rgba(191, 219, 254, 0.85);
}

.panel-meta {
  padding: 0.15rem 0.65rem;
  border-radius: 999px;
  background: rgba(59, 130, 246, 0.18);
  color: rgba(191, 219, 254, 0.85);
  font-size: 0.75rem;
}

.panel-body {
  flex: 1;
  min-height: 0;
  min-width: 0;
  overflow-y: auto;
}

.contacts-panel .panel-body,
.discovery-panel .panel-body {
  scrollbar-width: none;
  -ms-overflow-style: none;
}

.contacts-panel .panel-body::-webkit-scrollbar,
.discovery-panel .panel-body::-webkit-scrollbar {
  display: none;
}

.discovery-body {
  overflow: hidden;
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
}

.discovery-empty {
  color: rgba(148, 163, 184, 0.85);
  font-size: 0.85rem;
}

.discovery-items {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  max-height: 100%;
  overflow-y: auto;
  padding-right: 0.25rem;
}

.discovery-items li {
  display: grid;
  grid-template-columns: 1fr auto;
  gap: 0.65rem;
  align-items: center;
  padding: 0.65rem 0.75rem;
  border-radius: 12px;
  background: rgba(15, 23, 42, 0.55);
  border: 1px solid rgba(148, 163, 184, 0.1);
  transition: border-color 0.2s ease, transform 0.2s ease;
  position: relative;
}

.discovery-items li:hover .discovery-overlay {
  opacity: 1;
  transform: translateY(0);
}

.discovery-items li.active {
  border-color: rgba(59, 130, 246, 0.4);
}

.discovery-items li:hover {
  transform: translateY(-2px);
  border-color: rgba(148, 163, 184, 0.35);
}

.discovery-info {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
  min-width: 0;
}

.discovery-name {
  font-weight: 600;
  color: rgba(226, 232, 240, 0.95);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  cursor: pointer;
}

.discovery-meta {
  font-size: 0.75rem;
  color: rgba(148, 163, 184, 0.85);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.discovery-actions {
  display: flex;
  gap: 0.5rem;
}

.discovery-actions .invite-btn {
  border-radius: 10px;
  border: 1px solid rgba(16, 185, 129, 0.35);
  background: rgba(16, 185, 129, 0.18);
  color: #bbf7d0;
  padding: 0.35rem 0.75rem;
  font-size: 0.75rem;
  transition: background 0.2s ease, border-color 0.2s ease;
}

.discovery-actions .invite-btn:hover {
  border-color: rgba(16, 185, 129, 0.55);
  background: rgba(16, 185, 129, 0.28);
}

.discovery-actions .invite-btn:focus-visible {
  outline: 2px solid rgba(16, 185, 129, 0.6);
  outline-offset: 2px;
}

.discovery-tooltip {
  position: absolute;
  top: 0;
  left: 0;
  right: 0;
  padding: 0.75rem 1rem;
  border-radius: 12px;
  background: rgba(15, 23, 42, 0.85);
  border: 1px solid rgba(148, 163, 184, 0.25);
  box-shadow: 0 10px 30px rgba(15, 23, 42, 0.45);
  backdrop-filter: blur(8px);
  pointer-events: none;
  transform: translateY(0);
  opacity: 1;
  z-index: 5;
}

.discovery-tooltip ul {
  list-style: none;
  margin: 0;
  padding: 0.5rem 0 0 0;
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  font-size: 0.85rem;
  color: rgba(226, 232, 240, 0.9);
}

.discovery-tooltip li span {
  color: rgba(148, 163, 184, 0.95);
  margin-left: 0.25rem;
}
.node-panel .panel-body {
  overflow-y: hidden;
}

.panel-body::-webkit-scrollbar {
  width: 6px;
}

.panel-body::-webkit-scrollbar-thumb {
  background: rgba(148, 163, 184, 0.35);
  border-radius: 999px;
}

.node-body {
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
  position: relative;
}

.node-label {
  margin: 0;
  color: rgba(226, 232, 240, 0.9);
  font-weight: 600;
  line-height: 1.4;
  word-break: break-word;
}

.node-stats {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  color: rgba(148, 163, 184, 0.85);
  font-size: 0.85rem;
}

.node-stats li span {
  color: rgba(226, 232, 240, 0.92);
  margin-left: 0.35rem;
}

.node-info-overlay {
  position: absolute;
  top: 0;
  left: 0;
  right: 0;
  padding: 0.75rem 1rem;
  border-radius: 12px;
  background: rgba(15, 23, 42, 0.85);
  border: 1px solid rgba(148, 163, 184, 0.25);
  box-shadow: 0 10px 30px rgba(15, 23, 42, 0.45);
  backdrop-filter: blur(8px);
  opacity: 0;
  transform: translateY(-6px);
  transition: opacity 0.2s ease, transform 0.2s ease;
  pointer-events: none;
  min-width: 0;
  overflow: hidden;
}

.node-info-overlay ul {
  list-style: none;
  margin: 0;
  padding: 0.5rem 0 0 0;
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  font-size: 0.85rem;
  color: rgba(226, 232, 240, 0.9);
}

.node-info-overlay .overlay-title,
.discovery-overlay .overlay-title {
  margin: 0;
  font-size: 0.85rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: rgba(191, 219, 254, 0.9);
}

.node-info-overlay span {
  color: rgba(148, 163, 184, 0.95);
  margin-left: 0.25rem;
}

.discovery-overlay {
  position: absolute;
  top: 0;
  left: 0;
  right: 0;
  padding: 0.75rem 1rem;
  border-radius: 12px;
  background: rgba(15, 23, 42, 0.85);
  border: 1px solid rgba(148, 163, 184, 0.25);
  box-shadow: 0 10px 30px rgba(15, 23, 42, 0.45);
  backdrop-filter: blur(8px);
  opacity: 0;
  transform: translateY(-6px);
  transition: opacity 0.2s ease, transform 0.2s ease;
  pointer-events: none;
  min-width: 0;
  overflow: hidden;
}

.discovery-overlay ul {
  list-style: none;
  margin: 0;
  padding: 0.5rem 0 0 0;
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  font-size: 0.85rem;
  color: rgba(226, 232, 240, 0.9);
}

.discovery-overlay li {
  display: flex;
  flex-direction: row;
  gap: 0.25rem;
  word-break: break-word;
}

.node-panel:hover .node-info-overlay {
  opacity: 1;
  transform: translateY(0);
}

.node-meta-footer {
  margin-top: auto;
  display: flex;
  justify-content: flex-start;
  position: relative;
  z-index: 2;
}

.logout-button {
  margin-top: 0.75rem;
  align-self: flex-start;
  border-radius: 12px;
  padding: 0.65rem 1.2rem;
  border: none;
  background: rgba(239, 68, 68, 0.18);
  color: #fecdd3;
  font-weight: 600;
  letter-spacing: 0.05em;
  cursor: pointer;
  transition: background 0.2s ease;
  min-width: 100px;
}

.logout-button:hover {
  background: rgba(239, 68, 68, 0.25);
}

/* Responsive design for smaller screens */
@media (max-width: 1200px) {
  .chat-layout {
    grid-template-columns: minmax(250px, 2.5fr) minmax(80px, 1fr);
    gap: 1.25rem;
    padding: 1.25rem;
  }

  .sidebar {
    gap: 0.85rem;
  }

  .panel {
    min-height: 150px;
  }
}

@media (max-width: 768px) {
  .chat-layout {
    grid-template-columns: minmax(250px, 2fr) minmax(80px, 1fr);
    padding: 1rem;
    gap: 1rem;
  }

  .panel {
    min-height: 140px;
  }
}

/* Contact Request Popup Styles */
.contact-request-popup {
  position: fixed;
  top: 0;
  left: 0;
  width: 100%;
  height: 100%;
  background: rgba(0, 0, 0, 0.7);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
}

.popup-content {
  background: rgba(15, 23, 42, 0.95);
  backdrop-filter: blur(12px);
  border: 1px solid rgba(148, 163, 184, 0.3);
  border-radius: 24px;
  padding: 2rem;
  width: 90%;
  max-width: 400px;
  text-align: center;
}

.popup-content h3 {
  margin: 0 0 1rem 0;
  font-size: 1.4rem;
  color: #fff;
}

.popup-content p {
  margin: 0 0 1.5rem 0;
  color: rgba(255, 255, 255, 0.8);
}

.popup-actions {
  display: flex;
  gap: 1rem;
  justify-content: center;
}

.accept-btn {
  background: rgba(16, 185, 129, 0.8);
  color: white;
  border: none;
  border-radius: 12px;
  padding: 0.7rem 1.5rem;
  font-weight: 600;
  cursor: pointer;
  transition: background 0.2s;
}

.accept-btn:hover {
  background: rgba(34, 197, 94, 0.9);
}

.decline-btn {
  background: rgba(239, 68, 68, 0.2);
  color: #fecdd3;
  border: 1px solid rgba(239, 68, 68, 0.4);
  border-radius: 12px;
  padding: 0.7rem 1.5rem;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.2s;
}

.decline-btn:hover {
  background: rgba(239, 68, 68, 0.3);
  border-color: rgba(239, 68, 68, 0.6);
}

/* Contacts panel styles to match discovery panel */
.contacts-body {
  overflow: hidden;
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
}

.contacts-empty {
  color: rgba(148, 163, 184, 0.85);
  font-size: 0.85rem;
}

.contacts-items {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  max-height: 100%;
  overflow-y: auto;
  padding-right: 0.25rem;
}

.contacts-items li {
  display: grid;
  grid-template-columns: 1fr;
  gap: 0.65rem;
  align-items: center;
  padding: 0.65rem 0.75rem;
  border-radius: 12px;
  background: rgba(15, 23, 42, 0.55);
  border: 1px solid rgba(148, 163, 184, 0.1);
  transition: border-color 0.2s ease, transform 0.2s ease;
  position: relative;
  z-index: 1;
  cursor: pointer;
}

.contacts-items li.active {
  border-color: rgba(59, 130, 246, 0.4);
}

.contacts-items li:hover {
  transform: translateY(-2px);
  border-color: rgba(148, 163, 184, 0.35);
}

.contact-info {
  display: flex;
  align-items: center;
  gap: 0.75rem;
  min-width: 0;
}

.contact-avatar {
  width: 36px;
  height: 36px;
  border-radius: 50%;
  background: rgba(59, 130, 246, 0.25);
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 600;
  letter-spacing: 0.02em;
  flex-shrink: 0;
}

.contact-details {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
  min-width: 0;
  flex: 1;
}

.contact-name {
  font-weight: 600;
  color: rgba(226, 232, 240, 0.95);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.contact-meta {
  font-size: 0.75rem;
  color: rgba(148, 163, 184, 0.85);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.contact-status {
  font-size: 0.7rem;
  padding: 0.25rem 0.6rem;
  border-radius: 999px;
  border: 1px solid rgba(148, 163, 184, 0.2);
  flex-shrink: 0;
}

.contact-status.online {
  color: #86efac;
  border-color: rgba(34, 197, 94, 0.4);
}

.contact-status.offline {
  color: rgba(148, 163, 184, 0.9);
}

/* Ensure contact overlay behaves like discovery overlay */
.contacts-items li:hover .node-info-overlay {
  opacity: 1;
  transform: translateY(0);
  z-index: 10;
}

/* Smaller contact status indicator */
.contact-status-icon {
  width: 10px;
  height: 10px;
  border-radius: 50%;
  display: inline-block;
  flex-shrink: 0;
}

.contact-status-icon.online {
  background: #4ade80; /* Green for online */
  box-shadow: 0 0 6px rgba(74, 222, 128, 0.6);
}

.contact-status-icon.offline {
  background: #94a3b8; /* Gray for offline */
}
</style>
