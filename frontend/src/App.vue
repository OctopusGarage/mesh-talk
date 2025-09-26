<template>
  <div class="app-container">
    <RouterView />
    <AppFeedback />
  </div>
</template>

<script setup>
import AppFeedback from "./components/common/AppFeedback.vue";
</script>

<style scoped>
.app-container {
  width: 100%;
  min-width: 350px; /* Match global minimum width */
  height: 100vh;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

:host,
:host * {
  box-sizing: border-box;
}
</style>
