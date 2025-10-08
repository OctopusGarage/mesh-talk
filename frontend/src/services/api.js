import { invoke } from "@tauri-apps/api/core";

// Authentication API functions
const authAPI = {
  login: (username, password) => invoke("login", { username, password }),
  register: (username, password) => invoke("register", { username, password }),
  logout: () => invoke("logout"),
};

// Node API functions
const nodeAPI = {
  getNodeInfo: () => invoke("get_node_info"),
  connectToNode: (address) => invoke("connect_to_node", { address }),
  connectToUser: (userId) => invoke("connect_to_user", { userId }),
};

// Contact API functions
const contactsAPI = {
  getContacts: () => invoke("get_contacts"),
  updateContact: (contactId, data) =>
    invoke("update_contact", { id: contactId, ...data }),
  deleteContact: (contactId) => invoke("delete_contact", { contactId }),
  sendContactRequest: (targetPublicKey, alias, username, remoteIp, port, userId) => {
    console.log("[API] Sending contact request with parameters:", {
      targetPublicKey,
      alias,
      username,
      remoteIp,
      port,
      userId
    });
    
    return invoke("send_contact_request", { 
      targetPublicKey, 
      alias, 
      username,
      remote_ip: remoteIp, 
      port, 
      user_id: userId 
    });
  },
  handleContactRequest: (requestJson, approve) =>
    invoke("handle_contact_request", { requestJson, approve }),
};

// Message API functions
const messagesAPI = {
  getMessages: () => invoke("get_messages"),
  sendMessage: (content, target = {}) =>
    invoke("send_message", {
      content,
      targetUserId: target.userId ?? target.user_id ?? null,
      targetAddress: target.address ?? null,
    }),
  markMessageRead: (messageId) => invoke("mark_message_read", { messageId }),
};

const filesAPI = {
  sendFile: (path, target = {}) =>
    invoke("send_file", {
      path,
      targetUserId: target.userId ?? target.user_id ?? null,
      targetAddress: target.address ?? null,
    }),
  resumeTransfer: (transferId) =>
    invoke("resume_file_transfer", { transferId }),
  cancelTransfer: (transferId) =>
    invoke("cancel_file_transfer", { transferId }),
  listTransfers: () => invoke("list_file_transfers"),
  acceptIncoming: (transferId, savePath) =>
    invoke("accept_incoming_file_transfer", { transferId, savePath }),
  rejectIncoming: (transferId) =>
    invoke("reject_incoming_file_transfer", { transferId }),
};

// Network API functions
const networkAPI = {
  getDiscoveredNodes: () => invoke("get_discovered_nodes"),
  startNodeDiscovery: () => invoke("start_node_discovery"),
  stopNodeDiscovery: () => invoke("stop_node_discovery"),
};

// Combined API service
export const API = {
  auth: authAPI,
  node: nodeAPI,
  contacts: contactsAPI,
  messages: messagesAPI,
  files: filesAPI,
  network: networkAPI,
};
