import Foundation

/// Holds the newest cursor move that has not yet reached the wire.
///
/// A drag emits a move per display refresh -- up to 120 a second -- while each send blocks a
/// serial queue on a QUIC write. Whenever the link is slower than the finger, unconditional
/// enqueueing builds a backlog that never drains for the rest of the drag, so the Mac cursor
/// replays where the finger USED to be.
///
/// Moves carry ABSOLUTE positions, so a superseded one contributes nothing: dropping it and
/// sending only the newest puts the cursor in exactly the same place, sooner. That is the whole
/// reason this is safe here and would not be for a relative-delta protocol.
///
/// Separated from the sender because the sender is iOS-only (it links the Rust bridge), and this
/// policy is the part worth exercising directly -- see `ios/Tools/CursorMappingCheck`.
final class MoveCoalescer<Move> {
    private var pending: Move?
    private let lock = NSLock()

    /// Records the newest move, discarding any earlier one still waiting. Safe to call from the
    /// touch thread while a send is in flight -- that overlap is the entire point.
    func stage(_ move: Move) {
        lock.lock()
        pending = move
        lock.unlock()
    }

    /// Removes and returns the newest staged move, or `nil` if an earlier flush already took it.
    func takeLatest() -> Move? {
        lock.lock()
        defer { lock.unlock() }
        let move = pending
        pending = nil
        return move
    }
}
