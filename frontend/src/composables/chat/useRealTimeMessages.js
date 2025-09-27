import { computed } from 'vue';
import { storeToRefs } from 'pinia';
import { useAppStore } from '../../stores/appStore';

/**
 * Real-time messaging composable for handling message events
 * Provides methods to start and stop message listeners
 */
export function useRealTimeMessages() {
  // Store and references
  const store = useAppStore();
  const { messages, networkStatus } = storeToRefs(store);

  // Computed properties
  const isConnected = computed(() => networkStatus.value === 'connected');

  // Message listener methods
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
