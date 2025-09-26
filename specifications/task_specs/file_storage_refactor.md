# Implementation Plan: File-Based Storage Refactor

## Task Overview

Refactor Mesh-Talk to use file-based storage instead of SQLite, implementing a decentralized storage system where each user has their own encrypted data directory.

## Requirements Analysis

Based on the refactor requirements, this implementation should provide:

- Decentralized file-based storage for all user data
- End-to-end encryption for all sensitive information
- User-specific data isolation through directory structure
- Public/private key-based identity management
- Portable user data that can be backed up and migrated
- Secure password-based encryption for user data

## Implementation Approach

### 1. Project Structure

```
data/
├── {username}/
│   ├── meta.dat          # Encrypted user metadata
│   ├── contacts/
│   │   ├── index.dat     # Encrypted contact list index
│   │   └── {public_key}.dat  # Individual contact details
│   └── chats/
│       ├── index.dat     # Encrypted chat session index
│       └── {public_key}.dat  # Chat history with specific contact
└── ...
```

### 2. Core Components

#### File Storage Manager
- Implement a file-based storage system replacing SQLite
- Handle directory creation and management for each user
- Manage file encryption/decryption operations
- Implement secure file access patterns

#### Encryption System
- Implement password-based encryption for user data files
- Generate and manage public/private key pairs for each user
- Implement message encryption using recipient's public key
- Handle secure key storage and retrieval

#### User Identity Management
- Generate unique user IDs (UUID) for each new user
- Manage public/private key pairs for identity verification
- Implement password hashing for authentication
- Handle user data initialization and migration

#### Contact Management
- Store contacts by their public keys to prevent impersonation
- Implement contact aliasing (user-defined names)
- Handle contact discovery through network protocols
- Manage contact request and approval workflows

#### Chat Storage
- Implement encrypted chat history storage
- Organize chats by contact public keys
- Handle message indexing for efficient retrieval
- Implement message encryption/decryption

### 3. Implementation Steps

#### Phase 1: Core File Storage Infrastructure
1. Create file storage manager module
2. Implement directory structure management
3. Add file encryption/decryption utilities
4. Create data serialization/deserialization system

#### Phase 2: User Identity and Authentication
1. Implement user registration workflow
2. Create public/private key generation
3. Implement password hashing and verification
4. Add user data initialization

#### Phase 3: Contact Management
1. Implement contact storage structure
2. Add contact discovery mechanisms
3. Create contact request/approval system
4. Implement contact aliasing

#### Phase 4: Chat System
1. Implement encrypted chat storage
2. Add message encryption/decryption
3. Create chat indexing system
4. Implement chat history retrieval

#### Phase 4: Integration and Testing
1. Replace SQLite dependencies with file-based storage
2. Update network protocols to use public key identities
3. Implement comprehensive test coverage
4. Add documentation and migration guides

### 4. Security Considerations

- All sensitive data must be encrypted at rest
- Private keys must never be stored in plaintext
- Implement secure password handling
- Use industry-standard encryption algorithms (AES-256, RSA/Ed25519)
- Implement secure random number generation
- Handle memory securely (clear sensitive data after use)

### 5. Performance Considerations

- Implement file caching to reduce disk I/O
- Use efficient serialization formats (bincode, etc.)
- Implement lazy loading for large data sets
- Add pagination for chat history retrieval
- Optimize encryption operations