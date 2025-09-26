
### Decentralized Local Network Chat Tool Refactoring Requirements

#### General Requirements

* **Storage Method**: No longer use SQLite, switch to **local file storage**, all sensitive data must be encrypted.
* **User-Dimension Storage**: Each user has an individual folder under the `data` directory, all information is stored in this folder.
* **Identity Mechanism**: The system generates a unique ID for each user and uses public/private key mechanism to ensure communication security.
* **User Migration**: User data folders can be backed up or migrated to other devices independently, unlocked with username + password.

---

#### Data Storage Structure

```
data/
   ├── user1/
   │     ├── user.file          # User information (username, unique ID, public/private key, password verification, etc.)
   │     ├── contact.file       # Contact list (contact ID, public key, remarks, etc.)
   │     └── chat_record.file   # Chat records (stored separately by contact)
   ├── user2/
   │     ├── user.file
   │     ├── contact.file
   │     └── chat_record.file
   └── ...
```

---

#### Functional Flow

1. **Login/Register**

   * Interface only requires input: `username + password`.
   * Existing user → Read corresponding folder and decrypt data.
   * New user → System generates unique ID, public/private key, and creates corresponding user folder with initial files.
   * All files must be encrypted to ensure they can only be used by entering the correct username and password.

2. **Contact Management**

   * System automatically discovers new users on the local network.
   * Can send friend requests, establish contact after the other party agrees.
   * Contacts are uniquely identified by **public key** to prevent impersonation.
   * Contact list supports remarks and deletion.

3. **Chat Function**

   * Communication channel established based on public/private key mechanism.
   * Messages stored in `chat_record.file`, separated by contact.
   * All records are encrypted to ensure privacy.



