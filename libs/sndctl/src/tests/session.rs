//! The session: judging HELLO, and taking the driver's reports only in turn.

use super::message::qemu_hello;
use super::std::vec::Vec;

use ferrix_linux_abi::socket::Width;
use ferrix_linux_abi::sound::{HwParams, Interval, Mask};
use ferrix_native_abi::rights::Rights;

use crate::message::{
    DIRECTION_CAPTURE, DIRECTION_PLAYBACK, Elapsed, Message, Offer, PORT_RIGHTS, Refusal,
};
use crate::pcm::{Effects, Stream, VERSION_1};
use crate::refine::ANY;
use crate::session::{Publication, Received, Session, judge};

fn publication() -> Publication {
    judge(&qemu_hello(), &[PORT_RIGHTS]).expect("QEMU's device is taken")
}

#[test]
fn qemus_device_publishes_its_playback_stream_and_leaves_out_the_other() {
    let publication = publication();
    assert_eq!(publication.stream, 0);
    assert_eq!(publication.config, VERSION_1);
    assert_eq!(publication.left_out, 1);
    let ready = publication.ready(0);
    assert_eq!(ready.published, 1);
    assert_eq!(ready.streams[0].rate, 48_000);
    assert_eq!(ready.streams[0].format, 2, "S16_LE");
    assert_eq!(ready.streams[0].channels, 2);
    assert_eq!(ready.streams[0].period_bytes, 3840);
    assert_eq!(ready.streams[0].buffer_bytes, 15360);

    // A device whose first stream records publishes its second.
    let mut hello = qemu_hello();
    hello.offers.swap(0, 1);
    assert_eq!(judge(&hello, &[PORT_RIGHTS]).map(|p| p.stream), Ok(1));
}

#[test]
fn a_hello_the_core_cannot_take_is_refused_for_its_reason() {
    let mut other = qemu_hello();
    other.version = 2;
    assert_eq!(judge(&other, &[PORT_RIGHTS]), Err(Refusal::Version));

    for rights in [
        &[][..],
        &[Rights::WRITE][..],
        &[PORT_RIGHTS, PORT_RIGHTS][..],
    ] {
        assert_eq!(
            judge(&qemu_hello(), rights),
            Err(Refusal::Hello),
            "{rights:?}"
        );
    }

    let broken: [fn(&mut Offer); 4] = [
        |offer| offer.direction = 2,
        |offer| offer.channels_min = 0,
        |offer| offer.channels_min = 3,
        |offer| offer.rates |= 1 << 20,
    ];
    for (index, breaks) in broken.into_iter().enumerate() {
        let mut hello = qemu_hello();
        breaks(&mut hello.offers[1]);
        assert_eq!(
            judge(&hello, &[PORT_RIGHTS]),
            Err(Refusal::Hello),
            "case {index}"
        );
    }
    let mut none = qemu_hello();
    none.streams = 0;
    assert_eq!(judge(&none, &[PORT_RIGHTS]), Err(Refusal::Hello));

    // Nothing plays 48 kHz: nothing to publish.
    let mut slow = qemu_hello();
    slow.offers[0].rates = 1 << 6;
    assert_eq!(judge(&slow, &[PORT_RIGHTS]), Err(Refusal::Nothing));
    let mut only_capture = qemu_hello();
    only_capture.offers[0].direction = DIRECTION_CAPTURE;
    assert_eq!(judge(&only_capture, &[PORT_RIGHTS]), Err(Refusal::Nothing));
    assert_eq!(qemu_hello().offers[0].direction, DIRECTION_PLAYBACK);
}

fn running() -> (Session, Stream, Vec<Message>) {
    let mut stream = Stream::new(VERSION_1);
    stream.open(Width::Bits64);
    let mut effects = Effects::default();
    let mut any = HwParams {
        flags: 0,
        masks: [Mask {
            bits: [u32::MAX; 8],
        }; 3],
        mres: [Mask { bits: [0; 8] }; 5],
        intervals: [ANY; 12],
        ires: [Interval {
            min: 0,
            max: 0,
            flags: 0,
        }; 9],
        rmask: u32::MAX,
        cmask: 0,
        info: 0,
        msbits: 0,
        rate_num: 0,
        rate_den: 0,
        fifo_size: 0,
        sync: [0; 16],
        reserved: [0; 48],
    };
    stream.hw_params(&mut any, &mut effects).expect("HW_PARAMS");
    stream.prepare(&mut effects).expect("PREPARE");
    stream.wrote(1000, 0, &mut effects).expect("a write");
    let session = Session::new(publication());
    let sent = session.messages(&effects).collect();
    (session, stream, sent)
}

#[test]
fn reports_in_turn_move_the_stream_and_out_of_turn_break_the_session() {
    let (mut session, mut stream, sent) = running();
    let Some(Message::Submit(first)) = sent.first().copied() else {
        panic!("a write sends SUBMIT first: {sent:?}");
    };
    let report = |sequence, stream| {
        Message::Elapsed(Elapsed {
            stream,
            sequence,
            played: true,
            latency_bytes: 0,
        })
    };
    let mut effects = Effects::default();
    assert_eq!(
        session.receive(&report(first.sequence, 0), &mut stream, 1, &mut effects),
        Ok(Received::Stream)
    );
    assert!(stream.hw_ptr() > 0);

    // A report about a stream not published breaks it for good.
    let (mut session, mut stream, sent) = running();
    let Some(Message::Submit(first)) = sent.first().copied() else {
        panic!("SUBMIT");
    };
    assert_eq!(
        session.receive(&report(first.sequence, 1), &mut stream, 1, &mut effects),
        Err(Refusal::Protocol)
    );
    assert!(session.is_broken());
    assert_eq!(
        session.receive(&report(first.sequence, 0), &mut stream, 1, &mut effects),
        Err(Refusal::Protocol),
        "and stays broken"
    );

    // STOPPED only after STOP, and a report after STOP is out of turn.
    let (mut session, mut stream, _) = running();
    assert_eq!(
        session.receive(&Message::Stopped, &mut stream, 1, &mut effects),
        Err(Refusal::Protocol)
    );
    let (mut session, mut stream, sent) = running();
    session.stop();
    let Some(Message::Submit(first)) = sent.first().copied() else {
        panic!("SUBMIT");
    };
    assert_eq!(
        session.receive(&report(first.sequence, 0), &mut stream, 1, &mut effects),
        Err(Refusal::Protocol)
    );
    let (mut session, mut stream, _) = running();
    session.stop();
    assert_eq!(
        session.receive(&Message::Stopped, &mut stream, 1, &mut effects),
        Ok(Received::Stopped)
    );
}

#[test]
fn effects_become_submits_then_a_halt() {
    let (session, mut stream, sent) = running();
    assert!(
        sent.iter()
            .all(|message| matches!(message, Message::Submit(_)))
    );
    let mut effects = Effects::default();
    stream.drop_stream(0, &mut effects).expect("DROP");
    let messages: Vec<Message> = session.messages(&effects).collect();
    assert_eq!(messages, [Message::Halt { stream: 0 }]);
}
