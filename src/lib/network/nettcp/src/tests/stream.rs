//! Carrying bytes: in order, in both directions, and in the quantities that
//! make a window matter.

use alloc::vec;
use alloc::vec::Vec;

use crate::conn::Config;
use crate::state::State;
use crate::tests::harness::{End, Link};

/// A body whose every byte says where it is, so a test that reassembles it
/// wrongly fails on the content and not only on the length.
fn body(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
}

#[test]
fn a_short_write_arrives_whole() {
    let mut link = Link::pair();
    link.settle();
    assert_eq!(link.client.write(b"hello"), 5);
    link.settle();
    assert_eq!(link.drain(End::Server), b"hello");
}

#[test]
fn bytes_flow_both_ways_at_once() {
    let mut link = Link::pair();
    link.settle();
    assert_eq!(link.client.write(b"question"), 8);
    assert_eq!(link.end(End::Server).write(b"answer"), 6);
    link.settle();
    assert_eq!(link.drain(End::Server), b"question");
    assert_eq!(link.drain(End::Client), b"answer");
}

#[test]
fn a_body_larger_than_the_window_arrives_in_order() {
    let mut link = Link::pair();
    link.settle();
    let sent = body(200_000);
    let received = link.transfer(End::Client, &sent);
    assert_eq!(received.len(), sent.len());
    assert_eq!(received, sent);
}

#[test]
fn a_write_never_exceeds_the_send_buffer() {
    let config = Config {
        send_capacity: 4_096,
        ..Config::default()
    };
    let mut link = Link::new(config, Config::default());
    link.settle();
    let offered = body(10_000);
    let taken = link.client.write(&offered);
    assert_eq!(taken, 4_096);
}

#[test]
fn a_reader_that_never_reads_closes_the_window() {
    let config = Config {
        receive_capacity: 8_192,
        ..Config::default()
    };
    let mut link = Link::new(Config::default(), config);
    link.settle();
    let sent = body(64_000);
    let _ = link.client.write(&sent);
    link.settle();

    // The server never read, so it holds close to its whole buffer and no
    // more. It stops a little short of the ceiling because a window smaller
    // than a segment is advertised as none at all, which is what keeps a peer
    // from being invited to send a segment carrying four bytes.
    let held = link.end(End::Server).receive_queued();
    assert!(
        (6_000..=8_192).contains(&held),
        "the receive queue held {held} of its 8192 bytes"
    );
    assert!(link.client.send_queued() > 0);

    // Reading opens the window and the rest follows.
    let mut received = Vec::new();
    for _ in 0..512 {
        received.extend(link.drain(End::Server));
        link.settle();
        if received.len() >= sent.len() {
            break;
        }
    }
    assert_eq!(received, sent);
}

#[test]
fn data_written_before_a_close_arrives_before_the_close_does() {
    let mut link = Link::pair();
    link.settle();
    let sent = body(30_000);
    assert_eq!(link.client.write(&sent), sent.len());
    link.client.close();
    link.settle();

    let mut received = Vec::new();
    for _ in 0..512 {
        received.extend(link.drain(End::Server));
        link.settle();
        if received.len() >= sent.len() {
            break;
        }
    }
    assert_eq!(received, sent);
    assert_eq!(link.end(End::Server).state(), State::CloseWait);
}

#[test]
fn peeking_leaves_the_bytes_where_they_were() {
    let mut link = Link::pair();
    link.settle();
    let _ = link.client.write(b"abcdef");
    link.settle();
    let mut seen = [0_u8; 3];
    assert_eq!(link.end(End::Server).peek(&mut seen), 3);
    assert_eq!(&seen, b"abc");
    assert_eq!(link.drain(End::Server), b"abcdef");
}

#[test]
fn writing_to_a_closed_end_takes_nothing() {
    let mut link = Link::pair();
    link.settle();
    link.client.close();
    link.settle();
    assert_eq!(link.client.write(b"too late"), 0);
}

#[test]
fn nagle_holds_small_writes_together_and_no_delay_does_not() {
    let mut chatty = Link::pair();
    chatty.settle();
    let opening = chatty.sent;
    for _ in 0..8 {
        let _ = chatty.client.write(b"x");
        chatty.settle();
    }
    let nagled = chatty.sent - opening;

    let eager = Config {
        no_delay: true,
        ..Config::default()
    };
    let mut prompt = Link::new(eager, Config::default());
    prompt.settle();
    let opening = prompt.sent;
    for _ in 0..8 {
        let _ = prompt.client.write(b"x");
        prompt.settle();
    }
    let prompt_segments = prompt.sent - opening;

    assert_eq!(prompt.drain(End::Server), vec![b'x'; 8]);
    assert_eq!(chatty.drain(End::Server), vec![b'x'; 8]);
    assert!(
        nagled <= prompt_segments,
        "Nagle sent {nagled} segments and TCP_NODELAY {prompt_segments}"
    );
}

#[test]
fn every_second_segment_is_acknowledged_at_once_and_a_lone_one_waits() {
    let eager = Config {
        no_delay: true,
        ..Config::default()
    };
    let mut link = Link::new(eager, Config::default());
    link.settle();
    let segment = usize::from(link.client.segment_size());

    let _ = link.client.write(&body(segment));
    link.exchange();
    assert!(
        link.client.in_flight() > 0,
        "one segment alone is held for the delayed acknowledgment"
    );
    link.settle();
    assert_eq!(
        link.client.in_flight(),
        0,
        "and acknowledged once the delayed acknowledgment fires"
    );

    let _ = link.client.write(&body(2 * segment));
    link.exchange();
    assert_eq!(
        link.client.in_flight(),
        0,
        "two segments are acknowledged without the clock moving"
    );
    assert_eq!(link.drain(End::Server).len(), 3 * segment);
}
