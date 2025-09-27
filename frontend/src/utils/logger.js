/**
 * Frontend logging utilities for Mesh-Talk application
 *
 * This module provides a unified logging interface for the frontend that integrates
 * with the Tauri backend logging system.
 */

import { info, warn, error, debug } from "@tauri-apps/plugin-log";

/**
 * Log an informational message
 * @param {string} message - The message to log
 * @param {Object} [context] - Optional context data to include in the log
 */
export async function logInfo(message, context = null) {
  try {
    const logMessage = context 
      ? `${message} | Context: ${JSON.stringify(context)}`
      : message;
    await info(logMessage);
  } catch (err) {
    console.info(`[LOG FALLBACK] INFO: ${message}`, context || '');
  }
}

/**
 * Log a warning message
 * @param {string} message - The warning message to log
 * @param {Object} [context] - Optional context data to include in the log
 */
export async function logWarn(message, context = null) {
  try {
    const logMessage = context 
      ? `${message} | Context: ${JSON.stringify(context)}`
      : message;
    await warn(logMessage);
  } catch (err) {
    console.warn(`[LOG FALLBACK] WARN: ${message}`, context || '');
  }
}

/**
 * Log an error message
 * @param {string|Error} message - The error message or Error object to log
 * @param {Object} [context] - Optional context data to include in the log
 */
export async function logError(message, context = null) {
  try {
    const errorMessage = message instanceof Error 
      ? `${message.message} | Stack: ${message.stack || 'No stack trace'}`
      : message;
      
    const logMessage = context 
      ? `${errorMessage} | Context: ${JSON.stringify(context)}`
      : errorMessage;
      
    await error(logMessage);
  } catch (err) {
    console.error(`[LOG FALLBACK] ERROR: ${message}`, context || '');
  }
}

/**
 * Log a debug message
 * @param {string} message - The debug message to log
 * @param {Object} [context] - Optional context data to include in the log
 */
export async function logDebug(message, context = null) {
  try {
    const logMessage = context 
      ? `${message} | Context: ${JSON.stringify(context)}`
      : message;
    await debug(logMessage);
  } catch (err) {
    console.debug(`[LOG FALLBACK] DEBUG: ${message}`, context || '');
  }
}

/**
 * Log a chat message event
 * @param {string} eventType - Type of chat event (sent, received, delivered, etc.)
 * @param {Object} messageData - Message data
 */
export async function logChatEvent(eventType, messageData) {
  await logInfo(`Chat event: ${eventType}`, {
    messageId: messageData.id,
    from: messageData.from_address,
    to: messageData.to_address,
    contentLength: messageData.content?.length || 0,
    timestamp: messageData.sent_at
  });
}

/**
 * Log a network event
 * @param {string} eventType - Type of network event (connected, disconnected, etc.)
 * @param {Object} eventData - Network event data
 */
export async function logNetworkEvent(eventType, eventData) {
  await logInfo(`Network event: ${eventType}`, eventData);
}

/**
 * Log an authentication event
 * @param {string} eventType - Type of auth event (login, logout, register, etc.)
 * @param {Object} authData - Authentication data
 */
export async function logAuthEvent(eventType, authData) {
  await logInfo(`Auth event: ${eventType}`, {
    userId: authData.userId,
    username: authData.username,
    timestamp: Date.now()
  });
}

/**
 * Log a contact event
 * @param {string} eventType - Type of contact event (added, removed, updated, etc.)
 * @param {Object} contactData - Contact data
 */
export async function logContactEvent(eventType, contactData) {
  await logInfo(`Contact event: ${eventType}`, {
    contactId: contactData.id,
    contactName: contactData.name,
    contactAddress: contactData.address
  });
}

// Export all functions as a single object for convenience
export const Logger = {
  info: logInfo,
  warn: logWarn,
  error: logError,
  debug: logDebug,
  chat: logChatEvent,
  network: logNetworkEvent,
  auth: logAuthEvent,
  contact: logContactEvent
};

export default Logger;