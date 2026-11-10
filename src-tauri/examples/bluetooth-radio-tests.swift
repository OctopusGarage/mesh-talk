// SPDX-License-Identifier: MIT
import Foundation

#if BLE_REHEARSAL_TEST
@main
struct BluetoothRadioTests {
    static func main() {
        func check(_ chunks: [Data], capacity: Int, valid: Bool = true,
                   expected: BatchAdmission.Result) {
            var slots = capacity
            var forwarded: [Data] = []
            var responses: [BatchAdmission.Result] = []
            BatchAdmission.accept(chunks, valid: valid, reserve: {
                guard slots > 0 else { return false }
                slots -= 1
                return true
            }, release: { slots += 1 }, enqueue: { forwarded.append($0) },
            respond: { responses.append($0) })
            precondition(responses.count == 1 && responses[0] == expected)
            if expected == .success {
                precondition(forwarded == chunks)
                precondition(slots == capacity - chunks.count)
            } else {
                precondition(forwarded.isEmpty)
                precondition(slots == capacity)
            }
        }
        let ordered = [Data([1, 2]), Data([3]), Data(repeating: 4, count: 20)]
        check(ordered, capacity: 3, expected: .success)
        check([Data([1]), Data()], capacity: 3, expected: .invalid)
        check([Data([1]), Data(repeating: 2, count: 21)], capacity: 3, expected: .invalid)
        check(ordered, capacity: 2, expected: .capacity)
        check(ordered, capacity: 0, expected: .capacity)
        check(ordered, capacity: 3, valid: false, expected: .invalid)
        check([], capacity: 3, expected: .invalid)
        check(Array(repeating: Data([1]), count: 65), capacity: 65, expected: .invalid)
        check(Array(repeating: Data([1]), count: 64), capacity: 64, expected: .success)
        print("BLE batch admission tests passed")
    }
}
#endif
