//! Stack for the recursion over a statement's tree.
//!
//! Building, planning and running a statement recurse over its tree, which
//! the parser holds to [`crate::parse::MAX_DEPTH`] levels, through
//! functions with large frames. Each of those recursive functions runs its
//! body through [`guard`], which continues on a new heap segment when less
//! than [`RED_ZONE`] of the thread's stack is left, as om-calc's evaluator
//! does: a statement within the limit fits on any thread from any call
//! depth, in debug and release builds. What still recurses unguarded
//! (dropping a tree, and small helpers that walk an expression) is bounded
//! by the limit and fits in the red zone: measured with statements at the
//! limit, no stretch between guards needs more than 50 KiB in a debug
//! build or 10 KiB in a release build.

/// The stack a guarded function starts with at least, which covers
/// everything it runs before the next guard.
const RED_ZONE: usize = 256 * 1024;

/// The size of each new segment.
const SEGMENT: usize = 2 * 1024 * 1024;

/// Runs `f` with at least [`RED_ZONE`] bytes of stack.
pub(crate) fn guard<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(RED_ZONE, SEGMENT, f)
}
