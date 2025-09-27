import { storeToRefs } from "pinia";
import { useAppStore } from "../../stores/appStore";

/**
 * Authentication composable for handling user authentication
 * Provides methods for login, logout, and registration
 */
export function useAuth() {
  // Store and references
  const store = useAppStore();
  const { isAuthenticated, user, loading, error } = storeToRefs(store);

  // Authentication methods
  const login = async (username, password) => store.login(username, password);
  const logout = async () => store.logout();
  const register = async (username, password) => 
    store.register(username, password);

  return {
    isAuthenticated,
    user,
    loading,
    error,
    login,
    logout,
    register,
    setError: store.setError,
  };
}
