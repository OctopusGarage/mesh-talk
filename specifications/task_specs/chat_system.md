# Implementation Plan: Chat System

## Task Overview

Implement the encrypted chat system for the Mesh-Talk refactor, including message storage, encryption, and retrieval features.

## Requirements Analysis

This component must provide:

- End-to-end encrypted message storage
- Message encryption using public key cryptography
- Efficient chat history retrieval with pagination
- Support for different message types
- Integration with the file storage system

## Implementation Approach

### 1. Project Structure

```
src/chat/
├── mod.rs              # Chat module entry point
├── message.rs          # Message data structures
├── storage.rs          # Chat storage operations
├── encryption.rs       # Message encryption utilities
└── errors.rs           # Chat-specific error types
```

### 2. Core Components

#### Message Data Structures
- Define message data structure with encryption support
- Implement different message types (text, file, etc.)
- Add message metadata (timestamps, status, etc.)
- Handle message serialization

#### Chat Storage
- Implement encrypted chat history storage
- Organize chats by contact public keys
- Implement message indexing for efficient retrieval
- Handle message persistence to file system

#### Encryption System
- Implement message encryption using recipient's public key
- Handle message decryption using user's private key
- Manage message signing for authenticity
- Implement secure key handling

#### Error Handling
- Define specific error types for chat operations
- Implement proper error propagation
- Add context to errors for debugging

### 3. Implementation Steps

#### Step 1: Message Data Structures
1. Define `Message` struct with encryption support
2. Implement different message types
3. Add message metadata fields
4. Create message serialization utilities

#### Step 2: Chat Storage
1. Implement chat file storage structure
2. Add message indexing for efficient retrieval
3. Implement message addition and retrieval
4. Add chat history persistence to file system

#### Step 3: Message Encryption
1. Implement message encryption with recipient's public key
2. Handle message decryption with user's private key
3. Add message signing for authenticity
4. Implement secure key handling

#### Step 4: Chat Management
1. Implement chat session management
2. Add message status tracking (sent, delivered, read)
3. Create chat history retrieval with pagination
4. Implement message search features

#### Step 5: Error Handling
1. Define comprehensive error types
2. Implement proper error propagation
3. Add context to errors for debugging
4. Create user-friendly error messages

### 4. Security Considerations

- All messages must be encrypted end-to-end
- Use strong encryption algorithms (RSA/Ed25519 + AES)
- Implement secure key exchange mechanisms
- Validate all message data to prevent injection attacks
- Use secure communication for message transmission

### 5. Performance Considerations

- Implement message indexing for fast retrieval
- Use efficient data structures for chat histories
- Cache frequently accessed chat data
- Implement pagination for large chat histories
- Use compression for large messages/files

### 6. Testing Requirements

- Unit tests for all chat operations
- Integration tests for message encryption
- Security tests for message validation
- Performance benchmarks for chat operations
- Network integration tests

### 7. API Design

```rust
pub enum MessageType {
    Text,
    File,
    // Other message types
}

pub struct Message {
    pub id: String,
    pub from_public_key: String,
    pub to_public_key: String,
    pub timestamp: u64,
    pub encrypted_content: String,
    pub message_type: MessageType,
    pub status: MessageStatus,
}

pub enum MessageStatus {
    Sent,
    Delivered,
    Read,
    Failed,
}

pub struct ChatManager {
    file_manager: FileManager,
    identity_manager: IdentityManager,
}

impl ChatManager {
    pub fn new(
        file_manager: FileManager, 
        identity_manager: IdentityManager
    ) -> Self { ... }
    
    pub fn send_message(
        &self, 
        username: &str, 
        recipient_public_key: &str, 
        content: &str,
        message_type: MessageType
    ) -> Result<Message, ChatError> { ... }
    
    pub fn receive_message(
        &self, 
        username: &str, 
        encrypted_message: &str
    ) -> Result<Message, ChatError> { ... }
    
    pub fn get_chat_history(
        &self, 
        username: &str, 
        contact_public_key: &str,
        limit: Option<usize>,
        offset: Option<usize>
    ) -> Result<Vec<Message>, ChatError> { ... }
    
    pub fn mark_message_read(
        &self, 
        username: &str, 
        message_id: &str
    ) -> Result<(), ChatError> { ... }
    
    pub fn delete_message(
        &self, 
        username: &str, 
        message_id: &str
    ) -> Result<(), ChatError> { ... }
}
```