//! What the ABI promises, held against itself and against `libs/linux-abi`.

use core::mem::{offset_of, size_of};

use ferrix_linux_abi::nr::{from_aarch64, from_arm, from_x86_64};

use crate::bootstrap::{
    INIT_HELLO_BYTES, INIT_HELLO_HANDLES, INIT_HELLO_VERSION, init_hello, init_hello_version,
};
use crate::handle::Handle;
use crate::nr::{self, ALL, FIRST, LAST, NativeCall};
use crate::rights::{Requested, Rights, SAME_RIGHTS};
use crate::signals::Signals;
use crate::status;
use crate::types::{
    IoMappingSpec, PROCESS_EXITED, PROCESS_KILLED, PROCESS_RUNNING, PortPacket, ProcessStatus,
    ReadActual,
};

#[test]
fn no_native_number_is_a_linux_number_on_any_architecture() {
    for number in FIRST..=LAST {
        assert_eq!(from_x86_64(number), None, "{number:#x} is an x86-64 call");
        assert_eq!(from_aarch64(number), None, "{number:#x} is an AArch64 call");
        assert_eq!(from_arm(number), None, "{number:#x} is an ARMv7-A call");
    }
}

#[test]
fn every_call_round_trips_through_its_number() {
    for call in ALL {
        let number = nr::number(call);
        assert!(nr::is_native(number), "{call:?} is outside the range");
        assert_eq!(nr::decode(number), Some(call), "{call:?} does not decode");
    }
}

#[test]
fn every_number_in_the_range_is_a_call_or_a_gap_and_no_call_is_listed_twice() {
    let decoded = (FIRST..=LAST).filter_map(nr::decode).count();
    assert_eq!(decoded, ALL.len(), "a number decodes to a call ALL omits");
    for (i, a) in ALL.iter().enumerate() {
        for b in ALL.iter().skip(i + 1) {
            assert_ne!(a, b, "{a:?} is listed twice");
        }
    }
}

#[test]
fn all_is_in_number_order() {
    let numbers = ALL.map(nr::number);
    assert!(numbers.is_sorted(), "ALL is out of order: {numbers:x?}");
}

#[test]
fn nothing_outside_the_range_decodes() {
    for number in [0, 1, FIRST - 1, LAST + 1, 0x0F_0000, usize::MAX] {
        assert!(!nr::is_native(number), "{number:#x} claimed as native");
        assert_eq!(nr::decode(number), None, "{number:#x} decoded");
    }
}

#[test]
fn pinning_sits_in_the_vmo_block() {
    assert_eq!(nr::decode(0x1025), Some(NativeCall::VmoPin));
    assert_eq!(nr::decode(0x1026), Some(NativeCall::VmoPinAddresses));
    assert_eq!(nr::decode(0x1048), Some(NativeCall::BlockRingCreate));
    assert_eq!(nr::decode(0x1049), Some(NativeCall::DeviceInfo));
    assert_eq!(nr::decode(0x104A), Some(NativeCall::DeviceQuiesce));
    assert_eq!(nr::decode(0x104F), Some(NativeCall::DeviceClock));
    assert_eq!(nr::decode(0x1027), None, "0x1027 stays free");
    assert!(
        !Rights::PIN.contains(Rights::TRANSFER),
        "a pin stays with the driver that made it"
    );
}

#[test]
fn process_creation_opens_its_block() {
    assert_eq!(nr::decode(0x1030), Some(NativeCall::ProcessCreate));
    assert_eq!(nr::decode(0x1031), Some(NativeCall::ProcessStart));
    assert_eq!(nr::decode(0x1032), Some(NativeCall::ProcessGive));
    assert_eq!(nr::decode(0x1033), Some(NativeCall::ProcessBootstrap));
    assert_eq!(nr::decode(0x1034), Some(NativeCall::ProcessStatus));
    for number in 0x1035..=0x1037 {
        assert_eq!(nr::decode(number), None, "{number:#x} was assigned");
    }
    assert_eq!(
        nr::decode(0x1029),
        Some(NativeCall::JobKill),
        "JobKill moved"
    );
}

#[test]
fn status_names_are_pairwise_distinct() {
    for (i, a) in status::ALL.iter().enumerate() {
        for b in status::ALL.iter().skip(i + 1) {
            assert_ne!(a, b, "two native failures share errno {}", a.0);
        }
    }
}

#[test]
fn a_handle_that_does_not_fit_is_refused_rather_than_truncated() {
    assert_eq!(
        Handle::from_register(0x1001),
        Handle(0x1001),
        "a real value"
    );
    assert_eq!(
        Handle::from_register(0x1_0000_1001),
        Handle::INVALID,
        "upper bits must not be dropped"
    );
    assert_eq!(Handle::from_register(u64::MAX), Handle::INVALID, "all ones");
    assert!(!Handle::INVALID.is_valid(), "zero names nothing");
}

#[test]
fn rights_only_shrink() {
    let held = Rights::CHANNEL;
    assert_eq!(Requested::Same.resolve(held), Some(held), "same");
    assert_eq!(
        Requested::Exactly(Rights::READ).resolve(held),
        Some(Rights::READ),
        "a subset"
    );
    assert_eq!(
        Requested::Exactly(Rights::READ | Rights::MAP).resolve(held),
        None,
        "MAP is not held"
    );
    assert_eq!(
        Requested::Exactly(Rights::NONE).resolve(Rights::NONE),
        Some(Rights::NONE),
        "nothing from nothing"
    );
}

#[test]
fn a_rights_request_with_an_unknown_bit_is_refused() {
    assert_eq!(
        Requested::from_register(u64::from(SAME_RIGHTS)),
        Some(Requested::Same),
        "the sentinel"
    );
    assert_eq!(
        Requested::from_register(u64::from(Rights::ALL.0)),
        Some(Requested::Exactly(Rights::ALL)),
        "every defined right"
    );
    assert_eq!(Requested::from_register(0x80), None, "bit 7 is not a right");
    assert_eq!(
        Requested::from_register(u64::from(SAME_RIGHTS | 1)),
        None,
        "the sentinel is exact"
    );
    assert_eq!(
        Requested::from_register(1 << 32),
        None,
        "wider than 32 bits"
    );
}

#[test]
fn default_rights_are_defined_rights() {
    for rights in [
        Rights::CHANNEL,
        Rights::PORT,
        Rights::VMO,
        Rights::JOB,
        Rights::INTERRUPT,
        Rights::IO_MAPPING,
        Rights::DEVICE,
    ] {
        assert!(rights.is_known(), "{rights:?} has an undefined bit");
    }
    assert!(!Rights::INTERRUPT.contains(Rights::DUPLICATE), "one owner");
    assert!(
        !Rights::IO_MAPPING.contains(Rights::DUPLICATE),
        "one driver"
    );
    assert!(
        Rights::INTERRUPT.contains(Rights::TRANSFER),
        "devmgr hands it on"
    );
}

#[test]
fn a_wait_for_an_undefined_signal_is_refused() {
    assert_eq!(
        Signals::from_register(u64::from(Signals::ALL.0)),
        Some(Signals::ALL),
        "every defined signal"
    );
    assert_eq!(
        Signals::from_register(1 << 4),
        Some(Signals::EMPTY),
        "bit 4 is a job's EMPTY"
    );
    assert_eq!(Signals::from_register(1 << 5), None, "bit 5");
    assert_eq!(Signals::from_register(1 << 40), None, "a high bit");
    assert!(
        (Signals::READABLE | Signals::PEER_CLOSED).intersects(Signals::PEER_CLOSED),
        "any, not all"
    );
}

#[test]
fn layouts_have_no_padding_and_match_on_every_target() {
    assert_eq!(size_of::<PortPacket>(), 32, "PortPacket");
    assert_eq!(offset_of!(PortPacket, key), 0, "key");
    assert_eq!(offset_of!(PortPacket, kind), 8, "kind");
    assert_eq!(offset_of!(PortPacket, signals), 12, "signals");
    assert_eq!(offset_of!(PortPacket, data), 16, "data");

    assert_eq!(size_of::<ReadActual>(), 8, "ReadActual");
    assert_eq!(offset_of!(ReadActual, handles), 4, "handles");

    assert_eq!(size_of::<IoMappingSpec>(), 16, "IoMappingSpec");
    assert_eq!(offset_of!(IoMappingSpec, len), 8, "len");

    assert_eq!(size_of::<Handle>(), 4, "Handle");

    assert_eq!(size_of::<ProcessStatus>(), 8, "ProcessStatus");
    assert_eq!(offset_of!(ProcessStatus, value), 4, "value");
}

#[test]
fn a_process_status_says_running_exited_or_killed_and_nothing_else() {
    assert_eq!(
        ProcessStatus::default().state,
        PROCESS_RUNNING,
        "a zeroed status reads as running"
    );
    let states = [PROCESS_RUNNING, PROCESS_EXITED, PROCESS_KILLED];
    for (i, a) in states.iter().enumerate() {
        for b in states.iter().skip(i + 1) {
            assert_ne!(a, b, "two states share {a}");
        }
    }
}

#[test]
fn quotas_follow_the_job_calls() {
    assert_eq!(nr::decode(0x102A), Some(NativeCall::JobForCgroup));
    assert_eq!(nr::decode(0x102B), Some(NativeCall::JobSetLimit));
    assert_eq!(nr::decode(0x102C), Some(NativeCall::JobGetQuota));
    for number in 0x102D..=0x102F {
        assert_eq!(nr::decode(number), None, "{number:#x} was assigned");
    }
}

#[test]
fn init_hello_is_eight_fixed_bytes() {
    assert_eq!(INIT_HELLO_BYTES, 8, "the header's length");
    assert_eq!(INIT_HELLO_HANDLES, 0, "version 1 carries no handle");
    assert_eq!(
        init_hello(),
        *b"FXIN\x01\x00\x00\x00",
        "magic, then 1 little-endian"
    );
    assert_eq!(init_hello_version(&init_hello()), Some(INIT_HELLO_VERSION));
}

#[test]
fn init_hello_is_recognised_and_nothing_else_is() {
    let mut later = init_hello().to_vec();
    later[4] = 2;
    later.extend_from_slice(b"what version 2 adds");
    assert_eq!(
        init_hello_version(&later),
        Some(2),
        "a later version, longer"
    );
    assert_eq!(init_hello_version(&init_hello()[..7]), None, "short");
    assert_eq!(init_hello_version(b""), None, "empty");
    assert_eq!(init_hello_version(b"FXIM\x01\x00\x00\x00"), None, "magic");
    assert_eq!(
        init_hello_version(b"FXIN\x00\x00\x00\x00"),
        None,
        "version zero"
    );
}
