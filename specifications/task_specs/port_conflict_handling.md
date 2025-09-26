# Task Specification: UDP/TCP Port Conflict Handling and Dynamic Port Allocation

## Overview

This task addresses the handling of port conflicts in the Mesh-Talk application, specifically:
1. UDP broadcast port conflicts
2. TCP listening port conflicts
3. Implementation of dynamic port allocation when conflicts occur

## Problem Statement

As described in the project info, there are two types of port conflicts that need to be handled:

1. **UDP Port Conflicts**: While UDP ports can be bound by multiple programs with `SO_REUSEADDR`/`SO_REUSEPORT`, there's a risk of "mixed" traffic when other programs use the same port. The application must distinguish its own protocol packets from others using message format/magic numbers.

2. **TCP Port Conflicts**: TCP is strict - only one process can listen on a specific address:port combination. If the default port is occupied, the application must handle this gracefully.

## Requirements

### UDP Broadcast Handling
- Use a fixed UDP broadcast port (e.g., 9999) for discovery
- Include a protocol header/magic number in all UDP broadcast messages
- Implement message validation to distinguish protocol packets from other traffic
- Handle cases where other applications are using the same broadcast port

### TCP Port Handling
- Attempt to bind to a default TCP port first (e.g., 7000)
- If the default port is occupied, dynamically allocate an available port using system assignment
- Include the actual TCP listening port in UDP broadcast messages
- Update peer connections when TCP port changes

### Protocol Design
- Add magic numbers/protocol headers to all UDP/TCP messages
- Include version information in discovery messages
- Implement heartbeat mechanism for node availability detection

## Implementation Plan

### Phase 1: Protocol Enhancement
1. Define magic numbers for protocol identification
2. Add protocol headers to all message types
3. Include version information in discovery messages

### Phase 2: UDP Broadcast Improvements
1. Implement message validation using magic numbers
2. Add error handling for mixed traffic scenarios
3. Enhance broadcast messages with additional node information

### Phase 3: TCP Port Management
1. Implement default port binding with fallback to dynamic allocation
2. Update UDP broadcast messages to include actual TCP port
3. Add mechanisms to handle TCP port changes

### Phase 4: Heartbeat and Node Management
1. Implement periodic broadcast heartbeat
2. Add node timeout detection
3. Update peer connection management

## Technical Considerations

### Message Format
All UDP broadcast messages should include:
```
[Magic Number: 4 bytes][Version: 1 byte][Message Type: 1 byte][Payload]
```

### Default Ports
- UDP Broadcast Port: 9999
- Default TCP Port: 7000

### Dynamic Port Allocation
Use `TcpListener::bind("0.0.0.0:0")` to let the system assign an available port.

## Testing Requirements

1. Test UDP broadcast with multiple instances using the same port
2. Test TCP binding with occupied default port
3. Verify dynamic port allocation works correctly
4. Test message validation and protocol identification
5. Verify heartbeat mechanism for node detection

## Success Criteria

1. Application can handle UDP port conflicts gracefully
2. Application can bind to TCP ports even when default is occupied
3. Protocol messages are properly identified and validated
4. Peer discovery works reliably in various network conditions
5. No port conflict errors cause application crashes