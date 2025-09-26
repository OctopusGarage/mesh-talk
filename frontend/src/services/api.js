import { invoke } from "@tauri-apps/api/core";

// Authentication API functions
export const authAPI = {
  login: (username, password) => invoke("login", { username, password }),
  register: (username, password) => invoke("register", { username, password }),
  logout: () => invoke("logout"),
};

// Node API functions
export const nodeAPI = {
  getNodeInfo: () => invoke("get_node_info"),
  connectToNode: (address) => invoke("connect_to_node", { address }),
};

// Contact API functions
export const contactsAPI = {
  getContacts: () => invoke("get_contacts"),
  updateContact: (contactId, data) => invoke("update_contact", { contactId, ...data }),
  deleteContact: (contactId) => invoke("delete_contact", { contactId }),
  sendContactRequest: (targetPublicKey, alias) => 
    invoke("send_contact_request", { targetPublicKey, alias }),
  handleContactRequest: (requestJson, approve) =>
    invoke("handle_contact_request", { requestJson, approve }),
};

// Message API functions
export const messagesAPI = {
  getMessages: () => invoke("get_messages"),
  sendMessage: (content) => invoke("send_message", { content }),
  markMessageRead: (messageId) => invoke("mark_message_read", { messageId }),
  markAllMessagesRead: () => invoke("mark_all_messages_read"),
};

// Network API functions
export const networkAPI = {
  getDiscoveredNodes: () => invoke("get_discovered_nodes"),
  startNodeDiscovery: () => invoke("start_node_discovery"),
  stopNodeDiscovery: () => invoke("stop_node_discovery"),
};

// Combined API service
export const API = {
  ...authAPI,
  ...nodeAPI,
  ...contactsAPI,
  ...messagesAPI,
  ...networkAPI,
};