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

use alloc::collections::BTreeSet;
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
pub fn reaches<N>(
    start: Vec<N>,
    identity: impl Fn(&N) -> usize,
    target: usize,
    mut children: impl FnMut(&N) -> Vec<N>,
    limit: usize,
) -> Reach {
    let mut seen = BTreeSet::new();
    let mut pending = Vec::new();
    let mut held = Vec::new();
    for node in start {
        let id = identity(&node);
        if let Some(answer) = admit(node, id, target, &mut seen, &mut pending, limit) {
            return answer;
        }
    }
    while let Some(node) = pending.pop() {
        for child in children(&node) {
            let id = identity(&child);
            if let Some(answer) = admit(child, id, target, &mut seen, &mut pending, limit) {
                return answer;
            }
        }
        held.push(node);
    }
    Reach::Clear
}

/// Offer one node to the walk: the answer, if it settles the walk.
fn admit<N>(
    node: N,
    id: usize,
    target: usize,
    seen: &mut BTreeSet<usize>,
    pending: &mut Vec<N>,
    limit: usize,
) -> Option<Reach> {
    if id == target {
        return Some(Reach::Found);
    }
    if seen.insert(id) {
        if seen.len() > limit {
            return Some(Reach::TooFar);
        }
        pending.push(node);
    }
    None
}
