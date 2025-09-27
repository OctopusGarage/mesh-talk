/**
 * Utility functions for handling network addresses and node display formatting
 */

/**
 * Split an address into IP and port components
 * @param {string} address - The address string in format "ip:port"
 * @returns {Array} An array containing [ip, port] or [address, undefined] if no port
 */
export function splitAddress(address) {
  if (!address) {
    return [address, undefined];
  }
  
  const parts = String(address).split(':');
  if (parts.length >= 2) {
    const port = Number(parts.pop());
    return [parts.join(':'), Number.isNaN(port) ? undefined : port];
  }
  return [address, undefined];
}

/**
 * Extract just the IP part from an address string
 * @param {string} address - The address string in format "ip:port"
 * @returns {string} The IP part of the address
 */
export function extractAddressIP(address) {
  if (!address) return address;
  
  try {
    // If it's in the format ip:port, just show the IP part
    const parts = address.split(':');
    if (parts.length >= 2) {
      return parts[0];
    }
    return address;
  } catch (e) {
    return address;
  }
}

/**
 * Build a formatted display label for nodes
 * @param {Object} overrides - Object with name, username, ip, and port properties
 * @returns {string} Formatted display label in format "name • username • ip:port"
 */
export function buildNodeDisplayLabel(overrides = {}) {
  const nodeName = (overrides.name ?? "mesh-node").toString().trim() || "mesh-node";
  const accountName = overrides.username ?? "Guest";
  const ipAddress = overrides.ip ?? "127.0.0.1";
  const portValue = overrides.port ?? 0;
  return `${nodeName} • ${accountName} • ${ipAddress}:${portValue}`;
}

/**
 * Get the message conversation key to identify which conversation a message belongs to
 * @param {Object} message - The message object
 * @param {string} userAddress - The current user's address for context
 * @returns {string|null} The conversation address or null if not determinable
 */
export function getMessageConversationKey(message, userAddress = null) {
  if (!message) {
    return null;
  }

  // If this message is from the current user, return the recipient address
  if (message.from_address === userAddress && message.to_address) {
    return message.to_address;
  }
  
  // If this message is from someone else, return the sender address
  if (message.from_address && message.from_address !== userAddress) {
    return message.from_address;
  }

  // Fallback to to_address if from_address doesn't match self
  if (message.to_address && message.to_address !== userAddress) {
    return message.to_address;
  }

  return null;
}



/**
 * Normalize a list of contacts
 * @param {Array} list - Array of contact objects
 * @param {Object} currentUser - Current user context (optional)
 * @returns {Array} Array of normalized contact objects
 */
export function normalizeContactList(list = [], currentUser = null) {
  return [...list]
    .map(contact => normalizeContact(contact, currentUser))
    .sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" }));
}

/**
 * Normalize a message object with proper defaults
 * @param {Object} message - The raw message object
 * @returns {Object} Normalized message object
 */
export function normalizeMessage(message) {
  if (!message) {
    return message;
  }

  return {
    ...message,
    sent_at: message.sent_at ?? Date.now(),
    delivered_at: message.delivered_at ?? null,
    read_at: message.read_at ?? null,
    status: message.status ?? 0,
  };
}

/**
 * Normalize a list of messages
 * @param {Array} list - Array of message objects
 * @returns {Array} Array of normalized message objects, sorted by timestamp
 */
export function normalizeMessageList(list = []) {
  return [...list]
    .map(normalizeMessage)
    .sort((a, b) => (a.sent_at ?? 0) - (b.sent_at ?? 0));
}

/**
 * Extract and format node label information from node data
 * @param {Object} node - Node object
 * @returns {string} Formatted label
 */
export function buildDiscoveredLabel(node) {
  const nodeName = node.name ?? node.node_name ?? "Unknown";
  const username = node.username ?? "Unknown";
  const [ipPart, portPart] = (() => {
    if (node.ip) {
      return [node.ip, node.listen_port ?? node.port ?? undefined];
    }
    if (node.address) {
      const parts = String(node.address).split(":");
      if (parts.length >= 2) {
        const port = Number(parts.pop());
        return [parts.join(":"), Number.isNaN(port) ? undefined : port];
      }
      return [node.address, undefined];
    }
    return ["127.0.0.1", undefined];
  })();
  const port = node.listen_port ?? node.port ?? portPart ?? 0;
  return `${nodeName} • ${username} • ${ipPart}:${port}`;
}