import Foundation

/// Stores the newest absolute cursor position that has not reached the control channel.
/// A staged position replaces any older pending position.
/// This prevents a backlog when input arrives faster than control-channel writes complete.
/// Replacement is safe because each move contains an absolute position.
/// The lock permits staging while a send is in progress.
final class MoveCoalescer<Move> {
    private var pending: Move?
    private let lock = NSLock()

    /// Stores `move` as the newest pending move.
    /// The lock permits calls while a send is in progress.
    func stage(_ move: Move) {
        lock.lock()
        pending = move
        lock.unlock()
    }

    /// Removes and returns the newest pending move.
    /// Returns `nil` when no move is pending.
    func takeLatest() -> Move? {
        lock.lock()
        defer { lock.unlock() }
        let move = pending
        pending = nil
        return move
    }
}
