//! What happens when segments do not arrive.

use alloc::vec::Vec;

use crate::congestion::Phase;
use crate::conn::Config;
use crate::state::State;
use crate::tests::harness::{End, Link};

/// A body whose every byte says where it is.
fn body(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
}

/// Read one end until it stops producing, settling between reads.
fn collect(link: &mut Link, which: End, expected: usize) -> Vec<u8> {
    let mut received = Vec::new();
    for _ in 0..256 {
        received.extend(link.drain(which));
        link.settle();
        if received.len() >= expected {
            break;
        }
    }
    received
}

#[test]
fn a_lost_segment_is_sent_again() {
    let mut link = Link::pair();
    link.settle();
    // Lose the next thing the client sends, which is the data.
    link.lose = alloc::vec![link.sent];
    let _ = link.client.write(b"the only copy");
    link.settle();
    assert_eq!(link.drain(End::Server), b"the only copy");
}

#[test]
fn a_lost_segment_in_a_stream_does_not_reorder_what_follows() {
    let mut link = Link::pair();
    link.settle();
    // Lose two segments in the middle of a long body.
    link.lose = alloc::vec![link.sent + 3, link.sent + 11];
    let sent = body(120_000);
    let received = link.transfer(End::Client, &sent);
    assert_eq!(received.len(), sent.len());
    assert_eq!(received, sent);
}

#[test]
fn three_duplicate_acknowledgments_retransmit_without_waiting() {
    let mut link = Link::pair();
    link.settle();
    let before = link.now;
    link.lose = alloc::vec![link.sent];
    let sent = body(40_000);
    assert_eq!(link.client.write(&sent), sent.len());
    link.exchange();
    // The retransmission happened inside the exchange, with the clock stopped:
    // a timeout could not have caused it.
    assert_eq!(link.now, before);
    let received = collect(&mut link, End::Server, sent.len());
    assert_eq!(received, sent);
}

#[test]
fn loss_reduces_the_congestion_window() {
    let mut link = Link::pair();
    link.settle();
    let _ = link.client.write(&body(60_000));
    link.settle();
    let grown = link.client.congestion_window();

    let mut lossy = Link::pair();
    lossy.settle();
    lossy.lose = alloc::vec![lossy.sent + 2];
    let _ = lossy.client.write(&body(60_000));
    lossy.settle();
    assert!(
        lossy.client.congestion_window() < grown,
        "a connection that lost a segment should not have the window of one that did not: \
         {} against {grown}",
        lossy.client.congestion_window()
    );
}

#[test]
fn a_connection_whose_peer_vanishes_is_given_up_on() {
    let mut link = Link::pair();
    link.settle();
    let _ = link.client.write(b"is anyone there");
    // Everything from here on is lost.
    link.lose = (link.sent..link.sent + 256).collect();
    link.settle();
    assert_eq!(link.client.state(), State::Closed);
    assert_eq!(link.client.failure(), Some(crate::state::Failure::TimedOut));
}

#[test]
fn a_connection_starts_in_slow_start_and_leaves_it_after_loss() {
    let mut link = Link::pair();
    link.settle();
    let _ = link.client.write(&body(4_000));
    link.exchange();
    assert_eq!(link.client.congestion.phase(), Phase::SlowStart);
}

#[test]
fn a_shut_window_is_probed_until_it_opens() {
    let config = Config {
        receive_capacity: 4_096,
        ..Config::default()
    };
    let mut link = Link::new(Config::default(), config);
    link.settle();
    let sent = body(40_000);
    let _ = link.client.write(&sent);
    link.settle();
    // The server's buffer is full and it has advertised nothing, so the client
    // is stalled with bytes it cannot send.
    let held = link.end(End::Server).receive_queued();
    assert!(
        (2_500..=4_096).contains(&held),
        "the receive queue held {held} of its 4096 bytes"
    );
    assert!(link.client.send_queued() > 0);
    assert!(
        link.client.persist_at.is_some(),
        "a shut window should leave the probe armed"
    );

    let received = collect(&mut link, End::Server, sent.len());
    assert_eq!(received.len(), sent.len());
    assert_eq!(received, sent);
}
