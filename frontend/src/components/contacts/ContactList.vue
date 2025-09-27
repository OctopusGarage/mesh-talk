<template>
  <aside class="contact-list">
    <header>
      <p v-if="showNetworkBadge" class="network" :class="networkStatus">
        {{ networkLabel }}
      </p>
    </header>
    <div v-if="!contacts.length" class="empty">{{ emptyMessage }}</div>
    <ul>
      <li
        v-for="contact in contacts"
        :key="contact.id ?? contact.address"
        :class="{ active: contact.address === activeConversation }"
        @click="$emit('select', contact.address)"
      >
        <span class="avatar">{{ contact.name.charAt(0).toUpperCase() }}</span>
        <div class="details">
          <span class="name">{{ contact.name }}</span>
          <span class="address">{{ extractAddressIP(contact.address) }}</span>
        </div>
        <span class="status" :class="contact.status || 'offline'">
          {{ (contact.status || "offline").toUpperCase() }}
        </span>
      </li>
    </ul>
  </aside>
</template>

<script setup>
// Props and emits
import { computed } from "vue";
import { extractAddressIP } from "../../utils/addressUtils";
const props = defineProps({
  contacts: {
    type: Array,
    default: () => [],
  },
  networkStatus: {
    type: String,
    default: "disconnected",
  },
  activeConversation: {
    type: String,
    default: null,
  },
  title: {
    type: String,
    default: "Contacts",
  },
  emptyMessage: {
    type: String,
    default: "No contacts yet",
  },
  showNetworkBadge: {
    type: Boolean,
    default: true,
  },
});

defineEmits(["select"]);

// Computed properties
const networkLabel = computed(() => {
  if (props.networkStatus === "connected") return "Online";
  if (props.networkStatus === "connecting") return "Connecting";
  return "Offline";
});
</script>

<style scoped>
.contact-list {
  width: 100%;
  min-width: 0;
  background: rgba(15, 23, 42, 0.75);
  backdrop-filter: blur(12px);
  border: 1px solid rgba(148, 163, 184, 0.15);
  border-radius: 24px;
  display: flex;
  flex-direction: column;
  padding: 1.5rem;
  gap: 1rem;
  height: 100%;
}

header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

header h2 {
  margin: 0;
  font-size: 1.1rem;
  letter-spacing: 0.04em;
}

.network {
  font-size: 0.75rem;
  padding: 0.25rem 0.6rem;
  border-radius: 999px;
  text-transform: uppercase;
  letter-spacing: 0.08em;
}

.network.connected {
  background: rgba(34, 197, 94, 0.2);
  color: #bbf7d0;
}

.network.disconnected {
  background: rgba(239, 68, 68, 0.2);
  color: #fecdd3;
}

.network.connecting {
  background: rgba(234, 179, 8, 0.2);
  color: #fde68a;
}

.empty {
  color: rgba(148, 163, 184, 0.8);
  font-size: 0.9rem;
}

ul {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  overflow-y: auto;
  flex: 1;
}

li {
  display: grid;
  grid-template-columns: auto 1fr auto;
  align-items: center;
  gap: 0.75rem;
  padding: 0.75rem;
  background: rgba(15, 23, 42, 0.6);
  border-radius: 12px;
  border: 1px solid transparent;
  transition:
    border-color 0.2s,
    transform 0.2s;
}

li:hover {
  transform: translateY(-2px);
  border-color: rgba(148, 163, 184, 0.4);
}

li.active {
  border-color: rgba(59, 130, 246, 0.6);
}

.avatar {
  width: 36px;
  height: 36px;
  border-radius: 50%;
  background: rgba(59, 130, 246, 0.25);
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 600;
  letter-spacing: 0.02em;
}

.details {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
}

.name {
  font-weight: 600;
}

.address {
  font-size: 0.75rem;
  color: rgba(148, 163, 184, 0.9);
}

.status {
  font-size: 0.7rem;
  padding: 0.25rem 0.6rem;
  border-radius: 999px;
  border: 1px solid rgba(148, 163, 184, 0.2);
}

.status.online {
  color: #86efac;
  border-color: rgba(34, 197, 94, 0.4);
}

.status.offline {
  color: rgba(148, 163, 184, 0.9);
}
</style>
