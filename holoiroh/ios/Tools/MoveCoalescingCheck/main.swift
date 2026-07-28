import Foundation

// Witnesses the cursor-move send policy: the REAL `MoveCoalescer`, driven through the exact
// stage-then-flush pattern `FFIControlChannelSender.coalesceMove` uses, against a stand-in for
// the blocking bridge write. No device, no daemon, no network.

var failures = 0

func check(_ condition: Bool, _ what: String) {
    if condition {
        print("  ok   \(what)")
    } else {
        print("  FAIL \(what)")
        failures += 1
    }
}

/// Mirrors `FFIControlChannelSender`: a serial queue whose write blocks, fed from the touch
/// thread. `writeDelay` stands in for how long a QUIC write takes on the link under test.
final class SimulatedSender {
    private let queue = DispatchQueue(label: "witness.ffi")
    private let coalescer = MoveCoalescer<Int>()
    private let writeDelay: TimeInterval
    private var deliveredMoves: [Int] = []
    private let deliveredLock = NSLock()

    init(writeDelay: TimeInterval) {
        self.writeDelay = writeDelay
    }

    func send(_ move: Int) {
        coalescer.stage(move)
        queue.async {
            guard let latest = self.coalescer.takeLatest() else { return }
            if self.writeDelay > 0 {
                Thread.sleep(forTimeInterval: self.writeDelay)
            }
            self.deliveredLock.lock()
            self.deliveredMoves.append(latest)
            self.deliveredLock.unlock()
        }
    }

    func drain() -> [Int] {
        queue.sync {}
        deliveredLock.lock()
        defer { deliveredLock.unlock() }
        return deliveredMoves
    }
}

/// A drag emits a move per display refresh, so moves are staged at `stageInterval`, not in a
/// tight loop -- staging instantaneously would collapse everything to a single write and prove
/// nothing about how this behaves under a real finger.
func exercise(label: String, moves: Int, stageInterval: TimeInterval, writeDelay: TimeInterval) -> [Int] {
    let sender = SimulatedSender(writeDelay: writeDelay)
    for move in 1...moves {
        sender.send(move)
        Thread.sleep(forTimeInterval: stageInterval)
    }
    let delivered = sender.drain()
    print("\n\(label): staged \(moves), delivered \(delivered.count)")
    return delivered
}

let movesPerDrag = 60
let displayRefresh = 0.008

// A nearby LAN, which is what the user is asking to feel instant: every write finishes well
// inside a refresh, so each flush finds its own move and nothing is dropped at all.
let fast = exercise(
    label: "nearby LAN (write 0.5ms, finger 8ms)",
    moves: movesPerDrag,
    stageInterval: displayRefresh,
    writeDelay: 0.0005
)
check(
    fast.count >= movesPerDrag * 9 / 10,
    "a fast link delivers essentially every move (\(fast.count)/\(movesPerDrag)) -- coalescing costs nothing when it is not needed"
)
check(fast.last == movesPerDrag, "the final position is delivered")
check(fast == fast.sorted(), "delivered moves stay in staging order")

// A link too slow for the finger. Uncoalesced, these 60 moves would be 60 queued 25ms writes --
// 1.5s of backlog the cursor would spend replaying positions the finger already left.
let slow = exercise(
    label: "link slower than the finger (write 25ms, finger 8ms)",
    moves: movesPerDrag,
    stageInterval: displayRefresh,
    writeDelay: 0.025
)
check(
    slow.last == movesPerDrag,
    "the final position is delivered (\(slow.last.map(String.init) ?? "nothing")) -- the cursor never stops short of the finger"
)
check(
    slow == slow.sorted(),
    "delivered moves stay in staging order, so a stale position can never teleport the cursor backwards"
)
check(
    slow.count < movesPerDrag,
    "superseded moves are dropped rather than queued (\(slow.count) writes instead of \(movesPerDrag))"
)
check(
    Set(slow).count == slow.count,
    "no move is delivered twice"
)
check(
    fast.count > slow.count,
    "the policy self-clocks to the link (\(fast.count) delivered when fast vs \(slow.count) when slow) rather than capping the rate"
)

print("\nthe box itself")
let box = MoveCoalescer<Int>()
check(box.takeLatest() == nil, "an empty coalescer yields nothing")
box.stage(1)
box.stage(2)
box.stage(3)
check(box.takeLatest() == 3, "staging three times yields only the newest")
check(box.takeLatest() == nil, "a second take yields nothing, so a flush with no new move is a no-op")

print("\nthe sender actually uses this policy")
let sourceDir = ProcessInfo.processInfo.environment["HOLOIROH_SWIFT_SOURCES"] ?? ""
check(!sourceDir.isEmpty, "run.sh exported the source directory to scan")
let connectionSource = (try? String(contentsOfFile: "\(sourceDir)/HoloConnection.swift", encoding: .utf8)) ?? ""
check(!connectionSource.isEmpty, "HoloConnection.swift is readable")
check(
    connectionSource.contains("MoveCoalescer<ClientMessage>()"),
    "FFIControlChannelSender holds a coalescer"
)
check(
    connectionSource.contains("guard case .remoteControl(.move(_, _)) = message else"),
    "cursor moves take the coalescing path and everything else does not"
)
check(
    connectionSource.contains("self.moves.takeLatest()"),
    "each flush sends the newest staged move"
)

if failures == 0 {
    print("\nVERDICT: OK -- the cursor follows the finger instead of replaying a backlog, and never loses its final position")
    exit(0)
}
print("\nVERDICT: \(failures) FAILURE(S)")
exit(1)
