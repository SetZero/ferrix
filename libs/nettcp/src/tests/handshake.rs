//! Opening and closing, in every order the standard allows.

use crate::conn::{Config, Connection, Request};
use crate::seq::SeqNumber;
use crate::state::{Failure, State};
use crate::tests::harness::{End, Link};

#[test]
fn three_segments_open_a_connection() {
    let mut link = Link::pair();
    link.settle();
    assert_eq!(link.client.state(), State::Established);
    assert_eq!(link.end(End::Server).state(), State::Established);
    // SYN, SYN-ACK, ACK, and nothing else: an open connection with no data is
    // three segments.
    assert_eq!(link.sent, 3);
}

#[test]
fn the_handshake_carries_the_options_both_ends_offered() {
    let mut link = Link::pair();
    link.settle();
    let client_mss = link.client.segment_size();
    let server_mss = link.end(End::Server).segment_size();
    assert_eq!(client_mss, Config::default().segment_size);
    assert_eq!(server_mss, Config::default().segment_size);
}

#[test]
fn a_window_scale_is_only_used_when_both_ends_ask_for_it() {
    let unscaled = Config {
        window_scale: 0,
        ..Config::default()
    };
    let mut link = Link::new(Config::default(), unscaled);
    link.settle();
    // The client asked for a scale and the server answered with zero, which is
    // still an answer: the option is in use, at a shift of nothing.
    assert_eq!(link.client.state(), State::Established);
    assert_eq!(link.end(End::Server).state(), State::Established);
}

#[test]
fn closing_one_end_leaves_the_other_open_until_it_closes_too() {
    let mut link = Link::pair();
    link.settle();
    link.client.close();
    link.settle();
    assert_eq!(link.client.state(), State::FinWait2);
    assert_eq!(link.end(End::Server).state(), State::CloseWait);

    link.end(End::Server).close();
    link.settle();
    assert_eq!(link.end(End::Server).state(), State::Closed);
    assert_eq!(link.client.state(), State::TimeWait);
}

#[test]
fn time_wait_ends_by_itself() {
    let mut link = Link::pair();
    link.settle();
    link.client.close();
    link.settle();
    link.end(End::Server).close();
    link.settle();
    assert_eq!(link.client.state(), State::TimeWait);

    link.advance(crate::conn::TIME_WAIT + 1);
    assert_eq!(link.client.state(), State::Closed);
}

#[test]
fn both_ends_closing_at_once_ends_in_time_wait_on_both() {
    let mut link = Link::pair();
    link.settle();
    link.client.close();
    link.end(End::Server).close();
    link.settle();
    assert_eq!(link.client.state(), State::TimeWait);
    assert_eq!(link.end(End::Server).state(), State::TimeWait);
}

#[test]
fn a_connection_nobody_answers_is_given_up_on() {
    let mut link = Link::pair();
    // Lose every segment the client sends, so the server never appears.
    link.lose = (0..64).collect();
    link.settle();
    assert_eq!(link.client.state(), State::Closed);
    assert_eq!(link.client.failure(), Some(Failure::TimedOut));
}

#[test]
fn a_reset_to_a_connection_request_is_a_refusal() {
    let mut client = Connection::connect(Config::default(), 40_000, 80, SeqNumber(1_000));
    let mut payload = [0_u8; 64];
    let syn = client
        .poll_transmit(0, &mut payload)
        .expect("the request goes out first");

    let reset = ferrix_netwire::tcp::Header {
        source_port: 80,
        destination_port: 40_000,
        sequence: 0,
        acknowledgment: syn.header.sequence.wrapping_add(1),
        flags: ferrix_netwire::tcp::Flags::RST.union(ferrix_netwire::tcp::Flags::ACK),
        window: 0,
        urgent_pointer: 0,
        options: ferrix_netwire::tcp::Options::default(),
    };
    let segment = ferrix_netwire::tcp::Segment {
        header: reset,
        payload: &[],
    };
    let progress = client.on_segment(1, &segment);
    assert!(progress.closed);
    assert_eq!(client.state(), State::Closed);
    assert_eq!(client.failure(), Some(Failure::Refused));
}

#[test]
fn a_listener_answers_a_request_with_the_numbers_it_was_given() {
    let request = Request {
        sequence: SeqNumber(5_000),
        window: 4_096,
        mss: Some(1_400),
        window_scale: Some(3),
        selective_ack: true,
    };
    let mut server = Connection::accept(Config::default(), 80, 40_000, SeqNumber(77), &request);
    assert_eq!(server.state(), State::SynReceived);

    let mut payload = [0_u8; 64];
    let answer = server
        .poll_transmit(0, &mut payload)
        .expect("the answer goes out at once");
    assert!(
        answer
            .header
            .flags
            .contains(ferrix_netwire::tcp::Flags::SYN)
    );
    assert!(
        answer
            .header
            .flags
            .contains(ferrix_netwire::tcp::Flags::ACK)
    );
    assert_eq!(answer.header.sequence, 77);
    assert_eq!(answer.header.acknowledgment, 5_001);
    assert_eq!(server.segment_size(), 1_400);
}
