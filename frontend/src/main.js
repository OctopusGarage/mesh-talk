import { createApp } from "vue";
import { createPinia, storeToRefs } from "pinia";
import App from "./App.vue";
import router from "./router";
import "./styles/global.css";
import { useAppStore } from "./stores/appStore";
import { useFeedbackStore } from "./stores/feedbackStore";
import Logger from "./utils/logger";

// Initialize application
const app = createApp(App);
const pinia = createPinia();
app.use(pinia);

// Initialize stores
const feedback = useFeedbackStore();
const { isAuthenticated } = storeToRefs(useAppStore());

// Global error handling
window.addEventListener("error", (event) => {
  const payload = event?.error ?? event?.message;
  feedback.showError(payload, {
    message: event?.message ?? "Unexpected application error",
    title: "Application error",
  });
});

window.addEventListener("unhandledrejection", (event) => {
  feedback.showError(event?.reason, {
    message: "Unexpected background error",
    title: "Unhandled rejection",
  });
});

// Add global keyboard shortcuts
window.addEventListener("keydown", (event) => {
  // Cmd+Shift+A to open contact manager (macOS-style shortcut)
  if (event.metaKey && event.shiftKey && event.key === "A") {
    event.preventDefault();
    // We would need to communicate with the backend to open the contact manager
    // This is just a placeholder for now
    Logger.info("Cmd+Shift+A pressed - would open contact manager");
  }

  // Cmd+W to close window (macOS-style shortcut)
  if (event.metaKey && event.key === "w") {
    event.preventDefault();
    // We would need to communicate with Tauri to close the window
    // This is just a placeholder for now
    Logger.info("Cmd+W pressed - would close window");
  }
});

// Authentication guard
router.beforeEach((to, from, next) => {
  if (to.meta.requiresAuth && !isAuthenticated.value) {
    next({ name: "login", query: { redirect: to.fullPath } });
  } else if (to.name === "login" && isAuthenticated.value) {
    next({ name: "chat" });
  } else {
    next();
  }
});

app.use(router);
app.mount("#app");
