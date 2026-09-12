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
/// `identity` tells nodes apart, so a node reached along two paths is expanded
/// once and a loop in the graph ends the walk rather than repeating it.
/// `children` is called at most once per distinct node. Every node visited is
/// held until the walk returns, so an identity derived from where a node
/// lives stays unique for the whole walk.
pub fn reaches<N>(
    start: Vec<N>,
    identity: impl Fn(&N) -> usize,
    target: usize,
    mut children: impl FnMut(&N) -> Vec<N>,
    limit: usize,
) -> Reach {
    let mut seen = BTreeSet::new();
    let mut held = Vec::new();
    let mut pending = start;
    while let Some(node) = pending.pop() {
        let id = identity(&node);
        if id == target {
            return Reach::Found;
        }
        if !seen.insert(id) {
            continue;
        }
        if seen.len() > limit {
            return Reach::TooFar;
        }
        pending.extend(children(&node));
        held.push(node);
    }
    Reach::Clear
}
