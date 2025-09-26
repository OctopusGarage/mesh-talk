<template>
  <div class="feedback-layer" aria-live="polite" aria-atomic="true">
    <TransitionGroup name="toast">
      <article
        v-for="toast in toasts"
        :key="toast.id"
        class="toast"
        :data-type="toast.type"
      >
        <header class="toast__header">
          <span class="toast__title">{{ toast.title }}</span>
          <button
            type="button"
            class="toast__dismiss"
            aria-label="Dismiss notification"
            @click="dismiss(toast.id)"
          >
            ×
          </button>
        </header>
        <p class="toast__message">{{ toast.message }}</p>
        <pre v-if="toast.detail" class="toast__detail">{{ toast.detail }}</pre>
      </article>
    </TransitionGroup>

    <Transition name="fade">
      <section v-if="isBusy" class="backdrop" role="status">
        <div class="loader">
          <span class="spinner" aria-hidden="true" />
          <div class="loader__copy">
            <p>Working…</p>
            <ul>
              <li v-for="task in tasks" :key="task.key">{{ task.label }}</li>
            </ul>
          </div>
        </div>
      </section>
    </Transition>
  </div>
</template>

<script setup>
import { computed } from "vue";
import { useFeedbackStore } from "../../stores/feedbackStore";

const feedback = useFeedbackStore();
const toasts = computed(() => feedback.toasts);
const isBusy = computed(() => feedback.isBusy);
const tasks = computed(() => feedback.tasks);

const dismiss = (id) => feedback.dismissToast(id);
</script>

<style scoped>
.feedback-layer {
  pointer-events: none;
}

.toast-enter-active,
.toast-leave-active {
  transition: all 0.18s ease;
}

.toast-enter-from {
  opacity: 0;
  transform: translateY(-12px) scale(0.98);
}

.toast-leave-to {
  opacity: 0;
  transform: translateY(-12px) scale(0.98);
}

.toast {
  position: fixed;
  right: 1.5rem;
  top: 1.5rem;
  max-width: min(360px, 90vw);
  padding: 1rem 1.2rem;
  border-radius: 16px;
  border: 1px solid rgba(148, 163, 184, 0.18);
  background: rgba(15, 23, 42, 0.88);
  color: #e2e8f0;
  box-shadow: 0 14px 40px rgba(15, 23, 42, 0.3);
  margin-bottom: 0.75rem;
  pointer-events: auto;
}

.toast[data-type="success"] {
  border-color: rgba(34, 197, 94, 0.45);
  background: rgba(22, 101, 52, 0.8);
}

.toast[data-type="error"] {
  border-color: rgba(239, 68, 68, 0.5);
  background: rgba(127, 29, 29, 0.84);
}

.toast[data-type="warning"] {
  border-color: rgba(234, 179, 8, 0.45);
  background: rgba(120, 53, 15, 0.84);
}

.toast[data-type="info"] {
  border-color: rgba(59, 130, 246, 0.4);
  background: rgba(30, 64, 175, 0.82);
}

.toast__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  margin-bottom: 0.35rem;
}

.toast__title {
  font-weight: 600;
  letter-spacing: 0.04em;
}

.toast__dismiss {
  pointer-events: auto;
  background: transparent;
  border: none;
  color: inherit;
  font-size: 1.2rem;
  line-height: 1;
  cursor: pointer;
  opacity: 0.7;
  transition: opacity 0.15s ease;
}

.toast__dismiss:hover,
.toast__dismiss:focus {
  opacity: 1;
}

.toast__message {
  margin: 0;
  line-height: 1.4;
}

.toast__detail {
  margin: 0.6rem 0 0;
  padding: 0.6rem;
  font-size: 0.75rem;
  line-height: 1.3;
  background: rgba(15, 23, 42, 0.6);
  border-radius: 12px;
  border: 1px solid rgba(148, 163, 184, 0.25);
  overflow-x: auto;
  white-space: pre-wrap;
}

.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.2s ease;
}

.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}

.backdrop {
  position: fixed;
  inset: 0;
  backdrop-filter: blur(7px);
  background: rgba(15, 23, 42, 0.35);
  display: grid;
  place-items: center;
  pointer-events: auto;
  z-index: 10;
}

.loader {
  display: flex;
  gap: 1rem;
  align-items: center;
  background: rgba(15, 23, 42, 0.92);
  border-radius: 20px;
  padding: 1.4rem 1.8rem;
  border: 1px solid rgba(148, 163, 184, 0.2);
  color: #e2e8f0;
  min-width: min(320px, 90vw);
}

.spinner {
  width: 1.6rem;
  height: 1.6rem;
  border-radius: 50%;
  border: 3px solid rgba(148, 163, 184, 0.25);
  border-top-color: #38bdf8;
  animation: spin 0.7s linear infinite;
}

.loader__copy {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
}

.loader__copy p {
  margin: 0;
  font-weight: 600;
  letter-spacing: 0.04em;
}

.loader__copy ul {
  margin: 0;
  padding-left: 1.2rem;
  font-size: 0.85rem;
  color: rgba(226, 232, 240, 0.85);
}

@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

@media (prefers-reduced-motion: reduce) {
  .toast-enter-active,
  .toast-leave-active,
  .fade-enter-active,
  .fade-leave-active,
  .spinner {
    transition: none;
    animation: none;
  }
}
</style>
