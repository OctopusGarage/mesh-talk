<template>
  <section class="chat-window">
    <header>
      <div>
        <h2>{{ title }}</h2>
        <p>{{ subtitle }}</p>
      </div>
    </header>
    <div class="messages">
      <article
        v-for="message in messages"
        :key="message.id"
        :class="['message', messageDirection(message)]"
        @click="$emit('markRead', message.id)"
        @contextmenu.prevent="handleContextMenu($event, message)"
      >
        <template v-if="message.kind === 'file' && message.file">
          <div class="file-message">
            <div class="file-header">
              <span class="file-name">{{ message.file.fileName || '文件' }}</span>
              <span class="file-size">{{ formatBytes(message.file.fileSize) }}</span>
            </div>
            <div class="file-progress">
              <div
                class="file-progress-bar"
                :style="{ width: progressWidth(message) }"
              ></div>
            </div>
            <p class="file-status">
              {{ renderFileStatus(message) }}
            </p>
            <div class="file-actions">
              <button
                v-if="canResume(message)"
                type="button"
                class="file-action"
                @click.stop="$emit('resumeTransfer', message.file.transferId)"
              >
                继续
              </button>
              <button
                v-if="canCancel(message)"
                type="button"
                class="file-action"
                @click.stop="$emit('cancelTransfer', message.file.transferId)"
              >
                暂停
              </button>
              <button
                v-if="canOpen(message)"
                type="button"
                class="file-action"
                @click.stop="$emit('openTransfer', message.file)"
              >
                打开
              </button>
            </div>
          </div>
        </template>
        <template v-else>
          <span class="content" title="右键复制消息">
            {{ message.content }}
          </span>
        </template>
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

defineEmits(["markRead", "resumeTransfer", "cancelTransfer", "openTransfer"]);

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
  if (!props.currentUser || !message) {
    return "incoming";
  }

  const selfUserId = props.currentUser.user_id ?? props.currentUser.id ?? null;
  const selfAddress = props.currentUser.address ?? null;

  if (selfUserId && message.from_user_id === selfUserId) {
    return "outgoing";
  }

  if (selfAddress && message.from_address === selfAddress) {
    return "outgoing";
  }

  return "incoming";
};

const copyMessageContent = async (_event, content) => {
  if (!content) {
    return;
  }

  try {
    if (navigator?.clipboard?.writeText) {
      await navigator.clipboard.writeText(content);
    } else {
      const textarea = document.createElement("textarea");
      textarea.value = content;
      textarea.setAttribute("readonly", "true");
      textarea.style.position = "absolute";
      textarea.style.left = "-9999px";
      document.body.appendChild(textarea);
      textarea.select();
      document.execCommand("copy");
      document.body.removeChild(textarea);
    }
  } catch (err) {
    console.warn("[ChatWindow] Failed to copy message content", err);
  }
};

const handleContextMenu = (event, message) => {
  if (message?.kind === "file") {
    return;
  }
  copyMessageContent(event, message?.content);
};

const formatBytes = (bytes) => {
  const value = Number(bytes ?? 0);
  if (!Number.isFinite(value) || value <= 0) {
    return "0 B";
  }
  const units = ["B", "KB", "MB", "GB", "TB"];
  const exponent = Math.min(
    Math.floor(Math.log(value) / Math.log(1024)),
    units.length - 1,
  );
  const converted = value / 1024 ** exponent;
  return `${converted.toFixed(converted >= 10 ? 0 : 1)} ${units[exponent]}`;
};

const progressWidth = (message) => {
  if (!message?.file) {
    return "0%";
  }
  const total = message.file.fileSize || message.file.totalBytes || 0;
  if (!total) {
    return "0%";
  }
  const bytes = message.file.bytesTransferred ?? 0;
  const percent = Math.min(100, Math.floor((bytes / total) * 100));
  return `${percent}%`;
};

const renderFileStatus = (message) => {
  if (!message?.file) {
    return "";
  }
  const status = message.file.status ?? "pending";
  switch (status) {
    case "completed":
      return "传输完成";
    case "in_progress":
    case "pending":
      return `正在传输 · ${formatBytes(message.file.bytesTransferred)} / ${formatBytes(message.file.fileSize)}`;
    case "paused":
      return "已暂停";
    case "failed":
      return message.file.error || "传输失败";
    default:
      return status;
  }
};

const canResume = (message) => {
  if (!message?.file) {
    return false;
  }
  const status = message.file.status;
  return (
    message.file.direction === "outgoing" &&
    (status === "failed" || status === "paused")
  );
};

const canCancel = (message) => {
  if (!message?.file) {
    return false;
  }
  return (
    message.file.direction === "outgoing" &&
    (message.file.status === "in_progress" || message.file.status === "pending")
  );
};

const canOpen = (message) => {
  if (!message?.file) {
    return false;
  }
  return message.file.status === "completed" && Boolean(message.file.localPath || message.file.path);
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
  flex-direction: column;
  gap: 0.25rem;
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
  border: 1px solid transparent;
  box-shadow: 0 4px 12px rgba(15, 23, 42, 0.25);
}

.message.incoming {
  align-self: flex-start;
  background: linear-gradient(
    135deg,
    rgba(37, 99, 235, 0.35),
    rgba(59, 130, 246, 0.15)
  );
  border-color: rgba(59, 130, 246, 0.45);
  border-top-left-radius: 6px;
  border-bottom-left-radius: 6px;
  border-top-right-radius: 18px;
  margin-right: auto;
}

.message.outgoing {
  align-self: flex-end;
  background: linear-gradient(
    135deg,
    rgba(16, 185, 129, 0.35),
    rgba(45, 212, 191, 0.15)
  );
  border-color: rgba(16, 185, 129, 0.45);
  border-top-right-radius: 6px;
  border-bottom-right-radius: 6px;
  border-top-left-radius: 18px;
  margin-left: auto;
}

.content {
  white-space: pre-wrap;
  cursor: text;
}

.file-message {
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
}

.file-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 0.75rem;
  font-weight: 600;
}

.file-size {
  font-size: 0.75rem;
  color: rgba(226, 232, 240, 0.7);
}

.file-progress {
  width: 100%;
  height: 6px;
  border-radius: 999px;
  background: rgba(148, 163, 184, 0.25);
  overflow: hidden;
}

.file-progress-bar {
  height: 100%;
  background: linear-gradient(
    135deg,
    rgba(59, 130, 246, 0.8),
    rgba(14, 165, 233, 0.8)
  );
  transition: width 0.2s ease;
}

.file-status {
  margin: 0;
  font-size: 0.75rem;
  color: rgba(226, 232, 240, 0.85);
}

.file-actions {
  display: flex;
  gap: 0.5rem;
}

.file-action {
  background: rgba(59, 130, 246, 0.18);
  border: 1px solid rgba(59, 130, 246, 0.4);
  color: #dbeafe;
  padding: 0.35rem 0.75rem;
  border-radius: 999px;
  font-size: 0.75rem;
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
