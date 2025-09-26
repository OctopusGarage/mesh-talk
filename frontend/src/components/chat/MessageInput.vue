<template>
  <form class="message-input" @submit.prevent="emitMessage">
    <input
      v-model="draft"
      type="text"
      placeholder="Type a message"
      :disabled="disabled"
      @keydown="handleKeyDown"
    />
    <button type="submit" :disabled="disabled || !draft.trim()">Send</button>
  </form>
</template>

<script setup>
import { ref, watch } from 'vue';

const props = defineProps({
  disabled: {
    type: Boolean,
    default: false,
  },
});

const emit = defineEmits(['send']);

const draft = ref('');

const emitMessage = () => {
  const content = draft.value.trim();
  if (!content) return;
  emit('send', content);
  draft.value = '';
};

const handleKeyDown = (event) => {
  // Cmd+Enter or Ctrl+Enter to send message
  if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') {
    event.preventDefault();
    emitMessage();
  }
};

watch(
  () => props.disabled,
  (disabled) => {
    if (disabled) {
      draft.value = '';
    }
  },
);
</script>

<style scoped>
.message-input {
  display: grid;
  grid-template-columns: 1fr auto;
  gap: 1rem;
  align-items: center;
}

input {
  width: 100%;
  border-radius: 999px;
  border: 1px solid rgba(148, 163, 184, 0.3);
  background: rgba(15, 23, 42, 0.65);
  color: #e2e8f0;
  padding: 0.85rem 1.25rem;
}

button {
  background: linear-gradient(135deg, rgba(59, 130, 246, 0.9), rgba(14, 165, 233, 0.9));
  border: none;
  color: white;
  padding: 0.85rem 1.4rem;
  border-radius: 999px;
  font-weight: 600;
  letter-spacing: 0.04em;
}

button:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
