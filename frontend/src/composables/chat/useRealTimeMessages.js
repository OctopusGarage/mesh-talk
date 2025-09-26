import { computed } from 'vue';
import { storeToRefs } from 'pinia';
import { useAppStore } from '../../stores/appStore';

// Composable for real-time messaging functionality
export function useRealTimeMessages() {
  const store = useAppStore();
  const { messages, networkStatus } = storeToRefs(store);

  const isConnected = computed(() => networkStatus.value === 'connected');

  const startMessageListener = async () => {
    store.ensureEventListeners();
  };

  const stopMessageListener = async () => {
    await store.teardownEventListeners();
  };

  return {
    messages,
    isConnected,
    startMessageListener,
    stopMessageListener,
  };
}
