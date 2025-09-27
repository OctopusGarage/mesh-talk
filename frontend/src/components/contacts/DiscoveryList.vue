<template>
  <aside class="discovery-list">
    <div v-if="!nodes.length" class="empty">No nodes discovered</div>
    <ul>
      <li
        v-for="node in nodes"
        :key="node.address"
        @click="$emit('connect', node)"
        :class="{ active: node.address === activeAddress }"
      >
        <span class="avatar">{{ (node.name || node.address).charAt(0).toUpperCase() }}</span>
        <div class="details">
          <span class="name">{{ node.name }}</span>
          <span class="address">{{ node.username }}</span>
          <span class="address">{{ node.address }}</span>
        </div>
        <div class="node-actions">
          <span class="status" :class="node.status">{{ (node.status || 'offline').toUpperCase() }}</span>
          <button 
            class="invite-btn" 
            @click.stop="$emit('invite', node)"
            title="Invite to connect"
          >
            Invite
          </button>
        </div>
      </li>
    </ul>
  </aside>
</template>

<script setup>
// Props and emits
const props = defineProps({
  nodes: {
    type: Array,
    default: () => [],
  },
  activeAddress: {
    type: String,
    default: null,
  },
});

defineEmits(['connect', 'invite']);
</script>

<style scoped>
.discovery-list {
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

.discovery-list header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.discovery-list header h2 {
  margin: 0;
  font-size: 1.1rem;
  letter-spacing: 0.04em;
}

.meta {
  margin: 0;
  font-size: 0.75rem;
  padding: 0.25rem 0.6rem;
  border-radius: 999px;
  background: rgba(59, 130, 246, 0.15);
  color: rgba(191, 219, 254, 0.95);
  text-transform: uppercase;
  letter-spacing: 0.08em;
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
  transition: border-color 0.2s, transform 0.2s;
  cursor: pointer;
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

.node-actions {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 0.5rem;
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

.invite-btn {
  background: rgba(16, 185, 129, 0.2);
  color: #bbf7d0;
  border: 1px solid rgba(16, 185, 129, 0.4);
  border-radius: 8px;
  padding: 0.3rem 0.6rem;
  font-size: 0.75rem;
  cursor: pointer;
  transition: all 0.2s;
}

.invite-btn:hover {
  background: rgba(16, 185, 129, 0.3);
  border-color: rgba(16, 185, 129, 0.6);
}
</style>
