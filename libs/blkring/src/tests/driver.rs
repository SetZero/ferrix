//! The driver's checks: submissions (§4.3), indices (§4.2), the completion
//! ring's room, and a kernel scribbling on its fields.

use super::support::{device, read, with_pair};
use crate::Corruption;
use crate::driver::{CompleteError, Consumed};
use crate::geometry::{Device, DeviceFlags, InvalidSubmission, check_submission};
use crate::layout::{Op, RawSubmission, Status, Submission, header, submission};

#[test]
fn check_submission_refuses_everything_the_specification_refuses() {
    let fua = device();
    let plain = Device::new(512, 1 << 20, 64, DeviceFlags::FLUSH, 1 << 24).expect("valid");
    let good = Submission::write(1, 10, 4, 0).raw();
    let flush = Submission::flush(2).raw();
    let read = Op::Read.raw();
    let cases = [
        (
            RawSubmission { op: 0, ..good },
            &fua,
            InvalidSubmission::UnknownOp,
        ),
        (
            RawSubmission { op: 4, ..good },
            &fua,
            InvalidSubmission::UnknownOp,
        ),
        (
            RawSubmission {
                reserved: 1,
                ..good
            },
            &fua,
            InvalidSubmission::ReservedNotZero,
        ),
        (
            RawSubmission { flags: 2, ..good },
            &fua,
            InvalidSubmission::UnknownFlags,
        ),
        (
            RawSubmission {
                op: read,
                flags: 1,
                ..good
            },
            &fua,
            InvalidSubmission::FuaNotAllowed,
        ),
        (
            RawSubmission { flags: 1, ..flush },
            &fua,
            InvalidSubmission::FuaNotAllowed,
        ),
        (
            RawSubmission { flags: 1, ..good },
            &plain,
            InvalidSubmission::FuaNotAllowed,
        ),
        (
            RawSubmission { count: 0, ..good },
            &fua,
            InvalidSubmission::Empty,
        ),
        (
            RawSubmission { count: 1, ..flush },
            &fua,
            InvalidSubmission::FlushWithCount,
        ),
        (
            RawSubmission { count: 65, ..good },
            &fua,
            InvalidSubmission::TooManySectors,
        ),
        (
            RawSubmission {
                sector: (1 << 20) - 3,
                ..good
            },
            &fua,
            InvalidSubmission::PastCapacity,
        ),
        (
            RawSubmission {
                sector: u64::MAX,
                ..good
            },
            &fua,
            InvalidSubmission::PastCapacity,
        ),
        (
            RawSubmission {
                data_offset: (1 << 24) - 2047,
                ..good
            },
            &fua,
            InvalidSubmission::OutsideDataVmo,
        ),
        (
            RawSubmission {
                data_offset: u64::MAX,
                ..good
            },
            &fua,
            InvalidSubmission::OutsideDataVmo,
        ),
    ];
    for (raw, device, reason) in cases {
        assert_eq!(check_submission(&raw, device), Err(reason), "{raw:?}");
    }
}

#[test]
fn check_submission_accepts_the_edges_of_every_rule() {
    let fua = device();
    let good = Submission::write(1, 10, 4, 0).raw();
    let flush = Submission::flush(2).raw();
    let edges = [
        RawSubmission { flags: 1, ..good },
        RawSubmission {
            sector: (1 << 20) - 4,
            ..good
        },
        RawSubmission {
            data_offset: (1 << 24) - 2048,
            ..good
        },
        RawSubmission { count: 64, ..good },
        flush,
    ];
    for raw in edges {
        assert!(check_submission(&raw, &fua).is_ok(), "{raw:?}");
    }
}

#[test]
fn a_bad_submission_is_completed_refused_and_never_handed_to_the_glue() {
    with_pair(4, |kernel, driver, shared| {
        kernel.submit(read(1)).expect("room");
        kernel.submit(read(2)).expect("room");
        // The kernel's own checks stop a bad one, so spoil it once written.
        let at = kernel.layout().submission_at(0) + submission::OP;
        shared.poke(at, &[9]);
        let _ = kernel.publish();
        let refused = Consumed::Refused {
            id: 1,
            reason: InvalidSubmission::UnknownOp,
        };
        assert_eq!(driver.consume(), Ok(Some(refused)), "refused");
        assert_eq!(driver.held(), 0, "a refused request is not held");
        assert_eq!(
            driver.consume(),
            Ok(Some(Consumed::Request(read(2)))),
            "the next is fine"
        );
        driver.complete(2, Status::Ok, 512).expect("held");
        let _ = driver.publish();
        let first = kernel.poll().expect("honest").expect("pending");
        assert_eq!(
            (first.submission.id, first.status, first.bytes_done),
            (1, Status::Refused, 0),
            "1"
        );
        let second = kernel.poll().expect("honest").expect("pending");
        assert_eq!((second.submission.id, second.status), (2, Status::Ok), "2");
        assert_eq!(kernel.poll(), Ok(None), "no more");
        assert_eq!(kernel.outstanding(), 0, "both answered");
    });
}

#[test]
fn a_submission_tail_more_than_a_ring_ahead_is_corruption_and_latches() {
    with_pair(4, |_kernel, driver, shared| {
        shared.poke_u32(header::SUB_TAIL, 5);
        assert_eq!(
            driver.consume(),
            Err(Corruption::TailOverrun),
            "five in four"
        );
        assert_eq!(driver.consume(), Err(Corruption::TailOverrun), "latched");
        assert_eq!(
            driver.prepare_to_sleep(),
            Err(Corruption::TailOverrun),
            "latched"
        );
        assert_eq!(
            driver.complete(1, Status::Ok, 0),
            Err(CompleteError::Corrupt(Corruption::TailOverrun)),
            "latched"
        );
        assert_eq!(driver.publish(), None, "nothing published");
    });
}

#[test]
fn a_submission_tail_that_moves_backwards_is_corruption() {
    with_pair(4, |kernel, driver, shared| {
        for id in 1..=3 {
            kernel.submit(read(id)).expect("room");
        }
        let _ = kernel.publish();
        assert!(matches!(driver.consume(), Ok(Some(_))), "1");
        shared.poke_u32(header::SUB_TAIL, 2);
        assert_eq!(driver.consume(), Err(Corruption::TailBackwards), "3 then 2");
    });
}

#[test]
fn a_completion_head_past_the_tail_is_corruption() {
    with_pair(4, |kernel, driver, shared| {
        kernel.submit(read(1)).expect("room");
        let _ = kernel.publish();
        assert!(matches!(driver.consume(), Ok(Some(_))), "1");
        shared.poke_u32(header::COMP_HEAD, 1);
        assert_eq!(
            driver.complete(1, Status::Ok, 512),
            Err(CompleteError::Corrupt(Corruption::HeadOutOfRange)),
            "the kernel consumed a completion never published"
        );
    });
}

#[test]
fn a_completion_head_that_moves_backwards_is_caught_on_the_next_completion() {
    with_pair(4, |kernel, driver, shared| {
        for id in 1..=2 {
            kernel.submit(read(id)).expect("room");
        }
        let _ = kernel.publish();
        assert!(matches!(driver.consume(), Ok(Some(_))), "1");
        assert!(matches!(driver.consume(), Ok(Some(_))), "2");
        driver.complete(1, Status::Ok, 512).expect("held");
        let _ = driver.publish();
        assert!(matches!(kernel.poll(), Ok(Some(_))), "1");
        driver.complete(2, Status::Ok, 512).expect("sees head 1");
        let _ = driver.publish();
        kernel.submit(read(3)).expect("room");
        let _ = kernel.publish();
        assert!(matches!(driver.consume(), Ok(Some(_))), "3");
        shared.poke_u32(header::COMP_HEAD, 0);
        assert_eq!(
            driver.complete(3, Status::Ok, 512),
            Err(CompleteError::Corrupt(Corruption::HeadOutOfRange)),
            "1 then 0"
        );
    });
}

#[test]
fn a_kernel_with_more_than_entries_outstanding_is_caught_rather_than_waited_on() {
    with_pair(2, |kernel, driver, shared| {
        kernel.submit(read(1)).expect("room");
        kernel.submit(read(2)).expect("room");
        let _ = kernel.publish();
        assert!(matches!(driver.consume(), Ok(Some(_))), "1");
        assert!(matches!(driver.consume(), Ok(Some(_))), "2");
        driver.complete(1, Status::Ok, 512).expect("room");
        driver.complete(2, Status::Ok, 512).expect("room");
        let _ = driver.publish();
        // A kernel that ignores its bound: a third request while two
        // completions wait unread.
        let mut third = shared.memory(super::support::Who::Kernel, kernel.layout());
        read(3)
            .raw()
            .write_to(&mut third, kernel.layout().submission_at(2));
        shared.poke_u32(header::SUB_TAIL, 3);
        assert_eq!(
            driver.consume(),
            Ok(Some(Consumed::Request(read(3)))),
            "3 is well formed"
        );
        assert_eq!(
            driver.complete(3, Status::Ok, 512),
            Err(CompleteError::Corrupt(Corruption::Overcommitted)),
            "no room for what the driver holds"
        );
    });
}

#[test]
fn a_kernel_scribbling_on_the_drivers_fields_moves_nothing() {
    with_pair(4, |kernel, driver, shared| {
        for id in 1..=3 {
            kernel.submit(read(id)).expect("room");
        }
        let _ = kernel.publish();
        assert_eq!(driver.consume(), Ok(Some(Consumed::Request(read(1)))), "1");
        for at in [
            header::SUB_HEAD,
            header::COMP_TAIL,
            header::SUB_WANT_BELL,
            header::ENTRIES,
            header::SUB_OFFSET,
            header::COMP_OFFSET,
        ] {
            shared.poke_u32(at, 0xDEAD_BEEF);
        }
        for id in [2, 3] {
            assert_eq!(
                driver.consume(),
                Ok(Some(Consumed::Request(read(id)))),
                "{id}"
            );
        }
        assert_eq!(driver.consume(), Ok(None), "nothing more");
        assert_eq!(
            shared.peek_u32(header::SUB_HEAD),
            3,
            "the driver's own head"
        );
        for id in 1..=3 {
            driver.complete(id, Status::Ok, 512).expect("held");
        }
        let _ = driver.publish();
        assert_eq!(
            shared.peek_u32(header::COMP_TAIL),
            3,
            "the driver's own tail"
        );
        for id in 1..=3 {
            assert_eq!(
                kernel.poll().map(|c| c.map(|c| c.submission.id)),
                Ok(Some(id)),
                "{id}"
            );
        }
    });
}
