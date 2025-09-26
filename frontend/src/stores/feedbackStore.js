import { computed, reactive, ref } from "vue";
import { defineStore } from "pinia";

let toastId = 0;
let taskId = 0;

function defaultTitle(type) {
  switch (type) {
    case "success":
      return "Success";
    case "error":
      return "Something went wrong";
    case "warning":
      return "Attention";
    default:
      return "Notice";
  }
}

function normalizeError(error, fallbackMessage = "Unexpected error") {
  if (!error) {
    return { message: fallbackMessage };
  }

  if (typeof error === "string") {
    return { message: error };
  }

  if (error instanceof Error) {
    return {
      message: error.message || fallbackMessage,
      detail: error.stack,
      code: error.code,
      raw: error,
    };
  }

  if (typeof error === "object") {
    const message = error.message || error.error || fallbackMessage;
    const detail = error.detail || error.stack || undefined;
    return { message, detail, raw: error };
  }

  return { message: String(error) };
}

export const useFeedbackStore = defineStore("feedback", () => {
  const toasts = ref([]);
  const activeTasks = reactive(new Map());
  const lastError = ref(null);

  function createToast({
    message,
    type = "info",
    title = defaultTitle(type),
    detail,
    autoDismiss = 5000,
  }) {
    const id = ++toastId;
    const toast = {
      id,
      title,
      message,
      type,
      detail,
      createdAt: Date.now(),
      autoDismiss,
    };

    if (autoDismiss !== false && typeof window !== "undefined") {
      toast.timer = window.setTimeout(() => {
        dismissToast(id);
      }, autoDismiss);
    }

    toasts.value.push(toast);
    return id;
  }

  function dismissToast(id) {
    const index = toasts.value.findIndex((toast) => toast.id === id);
    if (index === -1) {
      return;
    }

    const [toast] = toasts.value.splice(index, 1);
    if (toast && toast.timer) {
      window.clearTimeout(toast.timer);
    }
  }

  function clearToasts() {
    toasts.value.forEach((toast) => {
      if (toast.timer) {
        window.clearTimeout(toast.timer);
      }
    });
    toasts.value = [];
  }

  function showSuccess(message, options = {}) {
    return createToast({ type: "success", message, ...options });
  }

  function showWarning(message, options = {}) {
    return createToast({ type: "warning", message, ...options });
  }

  function showInfo(message, options = {}) {
    return createToast({ type: "info", message, ...options });
  }

  function showError(error, options = {}) {
    const normalized = normalizeError(error, options.message);
    const toastMessage = options.message || normalized.message;
    const detail = options.detail ?? normalized.detail;

    lastError.value = {
      message: toastMessage,
      detail,
      raw: normalized.raw ?? error,
      code: normalized.code,
      context: options.context,
      occurredAt: new Date(),
    };

    if (options.toast !== false) {
      createToast({
        type: "error",
        title: options.title || defaultTitle("error"),
        message: toastMessage,
        detail,
        autoDismiss: options.autoDismiss ?? 8000,
      });
    }

    return lastError.value;
  }

  function clearLastError() {
    lastError.value = null;
  }

  function beginTask(key, label) {
    const taskKey = key || `task-${++taskId}`;
    activeTasks.set(taskKey, {
      key: taskKey,
      label: label || "Working…",
      startedAt: Date.now(),
    });
    return taskKey;
  }

  function endTask(key) {
    activeTasks.delete(key);
  }

  const isBusy = computed(() => activeTasks.size > 0);
  const tasks = computed(() => Array.from(activeTasks.values()));

  return {
    toasts,
    tasks,
    isBusy,
    lastError,
    createToast,
    dismissToast,
    clearToasts,
    showSuccess,
    showWarning,
    showInfo,
    showError,
    clearLastError,
    beginTask,
    endTask,
  };
});
