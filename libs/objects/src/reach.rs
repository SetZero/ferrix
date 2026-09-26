//! Whether one node can reach another, in a graph explored as it is walked.
//!
//! What a channel send asks before it queues an endpoint. Endpoints keep each
//! other alive only through their queues, so the graph of who keeps whom
//! alive has an edge from an endpoint to everything queued in it. A send adds
//! edges from the receiving end to what the message carries, and it closes a
//! cycle exactly when the receiving end is already reachable from one of
//! those. A cycle there is memory no close can free, so the send is refused
//! instead.
//!
//! # Bounded, and conservative about it
//!
//! A program can nest endpoints as deep as memory allows, and the walk runs
//! while other senders wait. So it stops after `limit` distinct nodes and
//! says [`Reach::TooFar`], which the caller refuses like a cycle: a false
//! refusal costs a program one send it had no business making that deep,
//! where a missed cycle costs the machine memory until it reboots.

use alloc::vec::Vec;

/// What a walk found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// The target is reachable.
    Found,
    /// Every node reachable was visited, and the target is not among them.
    Clear,
    /// More than `limit` distinct nodes are reachable, so the walk stopped
    /// without an answer.
    TooFar,
    /// There was no memory for the walk, so it stopped without an answer.
    NoMemory,
}

/// Whether the node identified by `target` is reachable from `start`.
///
/// `identity` tells nodes apart. A node is admitted, and counted against
/// `limit`, the first time it is offered, and never again: a node offered
/// along a thousand paths, or a thousand times by one parent, costs one slot
/// in the pending list and one call to `children`. Counting at admission
/// rather than at expansion is what bounds the pending list by `limit`, so a
/// parent with ten thousand copies of one child cannot fill memory while the
/// walk holds a lock. Every node admitted is held until the walk returns, so
/// an identity derived from where a node lives stays unique for the whole
/// walk.
///
/// `children` answers `None` when it could not list a node's children for
/// want of memory, and the walk then answers [`Reach::NoMemory`], as it does
/// when its own lists cannot grow: a walk that could not finish has no
/// answer, and a caller refuses the send as it would one too deep.
pub fn reaches<N>(
    start: Vec<N>,
    identity: impl Fn(&N) -> usize,
    target: usize,
    mut children: impl FnMut(&N) -> Option<Vec<N>>,
    limit: usize,
) -> Reach {
    // A sorted vector rather than a set: the walk is bounded by `limit`, and
    // a vector's growth can be refused, where a tree's cannot.
    let mut seen = Vec::new();
    let mut pending = Vec::new();
    let mut held = Vec::new();
    for node in start {
        let id = identity(&node);
        if let Some(answer) = admit(node, id, target, &mut seen, &mut pending, limit) {
            return answer;
        }
    }
    while let Some(node) = pending.pop() {
        let Some(next) = children(&node) else {
            return Reach::NoMemory;
        };
        for child in next {
            let id = identity(&child);
            if let Some(answer) = admit(child, id, target, &mut seen, &mut pending, limit) {
                return answer;
            }
        }
        if ferrix_fallible::try_push(&mut held, node).is_err() {
            return Reach::NoMemory;
        }
    }
    Reach::Clear
}

/// Mark `node` seen and queue it, unless it is the target or was seen
/// already. Returns an answer when the walk should stop.
fn admit<N>(
    node: N,
    id: usize,
    target: usize,
    seen: &mut Vec<usize>,
    pending: &mut Vec<N>,
    limit: usize,
) -> Option<Reach> {
    if id == target {
        return Some(Reach::Found);
    }
    let Err(at) = seen.binary_search(&id) else {
        return None;
    };
    if seen.len() >= limit {
        return Some(Reach::TooFar);
    }
    if ferrix_fallible::try_insert(seen, at, id).is_err()
        || ferrix_fallible::try_push(pending, node).is_err()
    {
        return Some(Reach::NoMemory);
    }
    None
}
