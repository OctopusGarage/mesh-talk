<template>
  <section class="chat-window">
    <header>
      <div>
        <h2>{{ title }}</h2>
        <p>{{ subtitle }}</p>
      </div>
      <button class="mark-read" @click="$emit('markAllRead')">
        Mark All Read
      </button>
    </header>
    <div class="messages">
      <article
        v-for="message in messages"
        :key="message.id"
        :class="['message', messageDirection(message)]"
        @click="$emit('markRead', message.id)"
      >
        <span class="content">{{ message.content }}</span>
        <time>{{ formatTimestamp(message.sent_at) }}</time>
      </article>
      <div v-if="!messages.length" class="empty">No messages yet.</div>
    </div>
    <footer>
      <slot name="input" />
    </footer>
  </section>
</template>

<script setup>
import { computed } from "vue";

// Props and emits
const props = defineProps({
  messages: {
    type: Array,
    default: () => [],
  },
  currentUser: {
    type: Object,
    default: null,
  },
  activeConversation: {
    type: String,
    default: null,
  },
  activeContact: {
    type: Object,
    default: null,
  },
});

defineEmits(["markRead", "markAllRead"]);

// Computed properties
const title = computed(() => {
  if (props.activeContact) {
    return (
      props.activeContact.username ||
      props.activeContact.name ||
      props.activeConversation ||
      "All Messages"
    );
  }
  return props.activeConversation ?? "All Messages";
});

const subtitle = computed(() =>
  props.activeConversation ? "Direct conversation" : "All activity",
);

// Utility functions
const formatTimestamp = (timestamp) => {
  if (!timestamp) return "";
  const date = new Date(timestamp * 1000);
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
};

const messageDirection = (message) => {
  if (!props.currentUser) return "incoming";
  return message.from_user_id === props.currentUser.id
    ? "outgoing"
    : "incoming";
};
</script>

<style scoped>
.chat-window {
  display: grid;
  grid-template-rows: auto 1fr auto;
  background: rgba(15, 23, 42, 0.6);
  backdrop-filter: blur(16px);
  border-radius: 24px;
  padding: 1.5rem;
  border: 1px solid rgba(148, 163, 184, 0.15);
  gap: 1.5rem;
  min-width: 250px;
  min-height: 250px;
  max-width: 100%;
  box-sizing: border-box;
}

header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 1rem;
}

header h2 {
  margin: 0;
  font-size: 1.2rem;
}

header p {
  margin: 0;
  color: rgba(148, 163, 184, 0.8);
  font-size: 0.85rem;
}

.mark-read {
  background: rgba(59, 130, 246, 0.25);
  border: 1px solid rgba(59, 130, 246, 0.45);
  border-radius: 999px;
  color: #dbeafe;
  padding: 0.4rem 0.9rem;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  font-size: 0.7rem;
}

.messages {
  display: flex;
  flex-direction: column;
  gap: 0.75rem;
  overflow-y: auto;
  min-height: 0;
}

.message {
  max-width: 70%;
  padding: 0.8rem 1rem;
  border-radius: 18px;
  line-height: 1.4;
  position: relative;
  display: inline-flex;
  flex-direction: column;
  gap: 0.35rem;
}

.message.incoming {
  align-self: flex-start;
  background: rgba(59, 130, 246, 0.25);
  border: 1px solid rgba(59, 130, 246, 0.35);
  border-bottom-left-radius: 4px;
}

.message.outgoing {
  align-self: flex-end;
  background: rgba(16, 185, 129, 0.25);
  border: 1px solid rgba(16, 185, 129, 0.35);
  border-bottom-right-radius: 4px;
}

.content {
  white-space: pre-wrap;
}

time {
  font-size: 0.7rem;
  color: rgba(148, 163, 184, 0.8);
  align-self: flex-end;
}

.empty {
  align-self: center;
  color: rgba(148, 163, 184, 0.6);
}

footer {
  display: flex;
  flex-direction: column;
}
</style>
