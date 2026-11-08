// SPDX-License-Identifier: MIT
// Experimental raw-byte BLE transport. Diagnostics use stderr exclusively.
import Foundation
import CoreBluetooth
import Darwin

// Admission is synchronous and nonblocking; enqueue owns each admitted slot.
enum BatchAdmission {
    enum Result { case success, invalid, capacity }
    static func accept(_ chunks: [Data], valid: Bool = true,
                       reserve: () -> Bool, release: () -> Void,
                       enqueue: (Data) -> Void, respond: (Result) -> Void) {
        guard valid, !chunks.isEmpty, chunks.count <= 64,
              chunks.allSatisfy({ !$0.isEmpty && $0.count <= 20 }) else {
            respond(.invalid)
            return
        }
        var reserved = 0
        for _ in chunks {
            guard reserve() else {
                for _ in 0..<reserved { release() }
                respond(.capacity)
                return
            }
            reserved += 1
        }
        for chunk in chunks { enqueue(chunk) }
        respond(.success)
    }
}

final class Radio: NSObject, CBCentralManagerDelegate, CBPeripheralDelegate, CBPeripheralManagerDelegate {
    let queue = DispatchQueue(label: "mesh-talk.ble")
    let permit = DispatchSemaphore(value: 0)
    let outputQueue = DispatchQueue(label: "mesh-talk.ble.output")
    let outputSlots = DispatchSemaphore(value: 64)
    let serviceID = CBUUID(string: "AC28D512-96E8-4520-A33A-43C71096A001")
    let rxID = CBUUID(string: "AC28D512-96E8-4520-A33A-43C71096A002")
    let txID = CBUUID(string: "AC28D512-96E8-4520-A33A-43C71096A003")
    let role: String
    var central: CBCentralManager!
    var manager: CBPeripheralManager!
    var peer: CBPeripheral?
    var subscriber: CBCentral?
    var rx: CBCharacteristic?
    var tx: CBMutableCharacteristic?
    var pending: Data?
    var active = true
    var ready = false
    var signalSources: [DispatchSourceSignal] = []
    var parentWatch: DispatchSourceTimer?

    init(role: String) {
        self.role = role
        super.init()
        for code in [SIGINT, SIGTERM, SIGHUP] {
            signal(code, SIG_IGN)
            let source = DispatchSource.makeSignalSource(signal: code, queue: queue)
            source.setEventHandler { self.stop(0, "interrupted") }
            signalSources.append(source)
            source.resume()
        }
        let ownerPID = getppid()
        guard ownerPID > 1 else { stop(1, "missing owning process"); return }
        let watch = DispatchSource.makeTimerSource(queue: queue)
        watch.schedule(deadline: .now() + 1, repeating: 1)
        watch.setEventHandler {
            if getppid() != ownerPID { self.stop(1, "owning process exited") }
        }
        parentWatch = watch
        watch.resume()
        queue.async {
            if role == "central" {
                self.log("initializing central")
                self.central = CBCentralManager(delegate: self, queue: self.queue)
                self.log("central initialized: \(self.central.state.rawValue)")
            } else {
                self.log("initializing peripheral")
                self.manager = CBPeripheralManager(delegate: self, queue: self.queue)
                self.log("peripheral initialized: \(self.manager.state.rawValue)")
            }
        }
        // Only this thread waits. A permit represents exactly one available packet slot.
        Thread.detachNewThread {
            while true {
                self.permit.wait()
                var bytes = [UInt8](repeating: 0, count: 20)
                let count = Darwin.read(STDIN_FILENO, &bytes, bytes.count)
                if count < 0 && errno == EINTR { self.permit.signal(); continue }
                let packet = count > 0 ? Data(bytes.prefix(count)) : nil
                self.queue.async {
                    guard self.active else { return }
                    guard let packet else { self.stop(count == 0 ? 0 : 1, "stdin closed"); return }
                    self.pending = packet
                    self.sendPending()
                }
                if count <= 0 { return }
            }
        }
    }
    func log(_ text: String) { FileHandle.standardError.write(Data((text + "\n").utf8)) }
    func stop(_ code: Int32, _ reason: String) {
        guard active else { return }
        active = false
        log(reason)
        central?.stopScan()
        if let peer { central?.cancelPeripheralConnection(peer) }
        manager?.stopAdvertising()
        manager?.removeAllServices()
        exit(code)
    }
    func markReady() {
        guard active, !ready else { return }
        ready = true
        log("ready: \(role)")
        permit.signal()
    }
    func output(_ data: Data) { outputBatch([data]) }
    func outputBatch(_ chunks: [Data]) {
        admit(chunks) { result in
            switch result {
            case .success: break
            case .invalid: self.stop(1, "invalid inbound BLE batch")
            case .capacity: self.stop(1, "inbound BLE budget exceeded")
            }
        }
    }
    func admit(_ chunks: [Data], valid: Bool = true, respond: (BatchAdmission.Result) -> Void) {
        BatchAdmission.accept(chunks, valid: valid,
            reserve: { self.outputSlots.wait(timeout: .now()) == .success },
            release: { self.outputSlots.signal() },
            enqueue: { data in self.outputQueue.async {
            defer { self.outputSlots.signal() }
            var offset = 0
            data.withUnsafeBytes { bytes in
                while offset < bytes.count {
                    let n = Darwin.write(STDOUT_FILENO, bytes.baseAddress!.advanced(by: offset), bytes.count - offset)
                    if n < 0 && errno == EINTR { continue }
                    if n <= 0 {
                        self.queue.async { self.stop(1, "stdout write failed") }
                        return
                    }
                    offset += n
                }
            }
        } }, respond: respond)
    }
    func sendPending() {
        guard active, let data = pending else { return }
        if role == "central" {
            guard let peer, let rx else { stop(1, "missing write characteristic"); return }
            peer.writeValue(data, for: rx, type: .withResponse)
        } else if let tx, let subscriber {
            if manager.updateValue(data, for: tx, onSubscribedCentrals: [subscriber]) {
                pending = nil
                permit.signal()
            }
        }
    }
    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        guard active else { return }
        if central.state == .poweredOn {
            log("scanning")
            central.scanForPeripherals(withServices: [serviceID])
        }
        else if ready || (central.state != .unknown && central.state != .resetting) { stop(1, "Bluetooth unavailable: \(central.state.rawValue)") }
    }
    func centralManager(_ central: CBCentralManager, didDiscover peripheral: CBPeripheral, advertisementData: [String: Any], rssi RSSI: NSNumber) {
        guard active, peer == nil else { return }
        peer = peripheral
        peripheral.delegate = self
        central.stopScan()
        central.connect(peripheral)
    }
    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        guard active else { return }
        peripheral.discoverServices([serviceID])
    }
    func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) { stop(1, "connection failed: \(String(describing: error))") }
    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?) { stop(1, "disconnected") }
    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard active else { return }
        guard error == nil, let service = peripheral.services?.first(where: { $0.uuid == serviceID }) else { stop(1, "service discovery failed"); return }
        peripheral.discoverCharacteristics([rxID, txID], for: service)
    }
    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        guard active else { return }
        let chars = service.characteristics ?? []
        guard error == nil, let write = chars.first(where: { $0.uuid == rxID }), write.properties.contains(.write),
              let notify = chars.first(where: { $0.uuid == txID }), notify.properties.contains(.notify) else { stop(1, "characteristic discovery failed"); return }
        rx = write
        peripheral.setNotifyValue(true, for: notify)
    }
    func peripheral(_ peripheral: CBPeripheral, didUpdateNotificationStateFor characteristic: CBCharacteristic, error: Error?) {
        guard active else { return }
        guard error == nil, characteristic.isNotifying else { stop(1, "notification subscription failed"); return }
        markReady()
    }
    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        guard active else { return }
        guard error == nil, let data = characteristic.value else { stop(1, "notification failed"); return }
        output(data)
    }
    func peripheral(_ peripheral: CBPeripheral, didWriteValueFor characteristic: CBCharacteristic, error: Error?) {
        guard active else { return }
        guard error == nil else { stop(1, "BLE write failed"); return }
        pending = nil
        permit.signal()
    }
    func peripheralManagerDidUpdateState(_ peripheral: CBPeripheralManager) {
        guard active else { return }
        if peripheral.state == .poweredOn {
            let service = CBMutableService(type: serviceID, primary: true)
            let receive = CBMutableCharacteristic(type: rxID, properties: [.write], value: nil, permissions: [.writeable])
            let transmit = CBMutableCharacteristic(type: txID, properties: [.notify], value: nil, permissions: [])
            tx = transmit
            service.characteristics = [receive, transmit]
            peripheral.add(service)
        } else if ready || (peripheral.state != .unknown && peripheral.state != .resetting) { stop(1, "Bluetooth unavailable: \(peripheral.state.rawValue)") }
    }
    func peripheralManager(_ peripheral: CBPeripheralManager, didAdd service: CBService, error: Error?) {
        guard active else { return }
        guard error == nil else { stop(1, "service registration failed"); return }
        peripheral.startAdvertising([CBAdvertisementDataServiceUUIDsKey: [serviceID]])
    }
    func peripheralManagerDidStartAdvertising(_ peripheral: CBPeripheralManager, error: Error?) {
        if error != nil { stop(1, "advertising failed") }
        else { log("advertising") }
    }
    func peripheralManager(_ peripheral: CBPeripheralManager, central: CBCentral, didSubscribeTo characteristic: CBCharacteristic) {
        guard active else { return }
        guard subscriber == nil || subscriber?.identifier == central.identifier else { stop(1, "multiple subscribers are unsupported"); return }
        subscriber = central
        peripheral.stopAdvertising()
        markReady()
    }
    func peripheralManager(_ peripheral: CBPeripheralManager, central: CBCentral, didUnsubscribeFrom characteristic: CBCharacteristic) { stop(1, "subscriber disconnected") }
    func peripheralManagerIsReady(toUpdateSubscribers peripheral: CBPeripheralManager) { sendPending() }
    func peripheralManager(_ peripheral: CBPeripheralManager, didReceiveWrite requests: [CBATTRequest]) {
        guard active else { return }
        guard let first = requests.first else { return }
        guard requests.count <= 64 else {
            peripheral.respond(to: first, withResult: .unlikelyError)
            return
        }
        let valid = requests.allSatisfy { request in
            request.characteristic.uuid == rxID && request.offset == 0 &&
            subscriber?.identifier == request.central.identifier && request.value != nil
        }
        // Respond once, only after whole-batch validation and admission.
        admit(requests.compactMap { $0.value }, valid: valid) { result in
            peripheral.respond(to: first, withResult: result == .success ? .success : .unlikelyError)
            if result == .capacity { self.stop(1, "inbound BLE budget exceeded") }
        }
    }
}

#if !BLE_REHEARSAL_TEST
@main
struct BluetoothRadioMain {
    static func main() {

        signal(SIGPIPE, SIG_IGN)
        guard CommandLine.arguments.count == 2, ["central", "peripheral"].contains(CommandLine.arguments[1]) else {
            FileHandle.standardError.write(Data("usage: bluetooth-radio central|peripheral\n".utf8))
            exit(2)
        }
        let radio = Radio(role: CommandLine.arguments[1])
        withExtendedLifetime(radio) { dispatchMain() }
    }
}
#endif
