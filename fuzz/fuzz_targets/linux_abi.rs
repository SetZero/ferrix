//! Fuzz the structures `libs/linux-abi` reads out of a program's memory.
//!
//! Every system call that takes a pointer hands the kernel bytes a stranger
//! wrote: a socket address, a `msghdr` and its control buffer, credentials,
//! the fixed headers of a netlink message, and the argument of every DRM,
//! evdev and virtgpu ioctl. This crate is where those bytes become values,
//! so the fuzzer's bytes are read as each of them in turn, at both pointer
//! widths.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **An address that parses encodes and parses back to itself**:
//!    `sockaddr_in`, `sockaddr_in6`, `sockaddr_un` in all three of its forms,
//!    and `sockaddr_nl`. The encoding is exactly as long as it says, and a
//!    buffer one byte shorter is refused rather than written short.
//! 2. **A record reads back from what it writes**: `msghdr`, `cmsghdr`,
//!    `ucred`, `linger`, the netlink headers, `drm_version` and
//!    `input_event` at both widths, and every structure defined with
//!    `layout!`.
//! 3. **A structure's fields lie inside it**: a buffer at least as long as a
//!    `layout!` structure always reads, so a field whose offset and type run
//!    past the size the probe printed is a crash here, not a short read in
//!    the kernel.
//! 4. **The control-message walk ends and stays inside its buffer**: every
//!    message is at least a header long, so a buffer of `n` bytes holds no
//!    more than `n / header + 1`; every message's data lies inside the
//!    buffer; and a refusal is the last thing a walk yields.
//! 5. **An evdev ioctl number decodes to what built it**, and the system
//!    call tables and the hardware capability bits answer any number and
//!    any register value.

#![no_main]

use core::fmt::Debug;

use ferrix_linux_abi::drm;
use ferrix_linux_abi::hwcap;
use ferrix_linux_abi::inet::InetAddress;
use ferrix_linux_abi::input;
use ferrix_linux_abi::layout::Layout;
use ferrix_linux_abi::netlink::{
    IfAddrMsg, IfInfoMsg, NdMsg, NetlinkAddress, NlAttr, NlMsgErr, NlMsgHdr, RtMsg,
};
use ferrix_linux_abi::nr;
use ferrix_linux_abi::socket::{
    CmsgHdr, ControlMessages, Linger, MsgHdr, Ucred, UnixAddress, Width,
};
use ferrix_linux_abi::virtgpu;
use libfuzzer_sys::fuzz_target;

/// Property 4: `inner` lies wholly inside `outer`.
fn inside(outer: &[u8], inner: &[u8]) -> bool {
    let outer = outer.as_ptr_range();
    let inner = inner.as_ptr_range();
    inner.start >= outer.start && inner.end <= outer.end
}

/// Property 1, for the three families a socket address comes in.
fn addresses(bytes: &[u8], addr_len: usize) {
    let mut out = [0_u8; 256];

    if let Ok(address) = InetAddress::parse(bytes, addr_len) {
        let len = address.encoded_len();
        assert_eq!(
            address.encode(&mut out),
            Some(len),
            "{address:?} did not encode"
        );
        assert_eq!(
            InetAddress::parse(&out, len).ok(),
            Some(address),
            "{address:?} did not parse back"
        );
        assert!(
            address.encode(&mut out[..len - 1]).is_none(),
            "{address:?} wrote short"
        );
    }

    if let Ok(address) = UnixAddress::parse(bytes, addr_len) {
        let len = address.encoded_len();
        assert_eq!(
            address.encode(&mut out),
            Some(len),
            "{address:?} did not encode"
        );
        assert_eq!(
            UnixAddress::parse(&out, len).ok(),
            Some(address),
            "{address:?} did not parse back"
        );
        assert!(
            address.encode(&mut out[..len - 1]).is_none(),
            "{address:?} wrote short"
        );
    }

    if let Ok(address) = NetlinkAddress::parse(bytes, addr_len) {
        let encoded = address.to_bytes();
        assert_eq!(
            NetlinkAddress::parse(&encoded, NetlinkAddress::SIZE).ok(),
            Some(address),
            "{address:?} did not parse back"
        );
    }
}

/// Property 2 for a record with `from_bytes` and `to_bytes`.
macro_rules! fixed_records {
    ($bytes:expr; $($record:ty),* $(,)?) => {$(
        if let Some(record) = <$record>::from_bytes($bytes) {
            assert_eq!(
                <$record>::from_bytes(&record.to_bytes()),
                Some(record),
                "{} did not read back", stringify!($record)
            );
        } else {
            assert!(
                $bytes.len() < <$record>::SIZE,
                "{} refused {} bytes", stringify!($record), $bytes.len()
            );
        }
    )*};
}

/// Properties 2 and 3 for a structure defined with `layout!`.
fn layout<T: Layout + PartialEq + Debug>(bytes: &[u8]) {
    for &(field, offset) in T::FIELDS {
        assert!(
            offset < T::SIZE,
            "{}.{field} is at {offset}, past its end",
            T::C_NAME
        );
    }
    let Some(value) = T::read(bytes) else {
        assert!(
            bytes.len() < T::SIZE,
            "{} refused {} bytes",
            T::C_NAME,
            bytes.len()
        );
        return;
    };
    let mut out = vec![0_u8; T::SIZE];
    assert!(
        value.write(&mut out).is_some(),
        "{} did not write",
        T::C_NAME
    );
    assert_eq!(
        T::read(&out),
        Some(value),
        "{} did not read back",
        T::C_NAME
    );
    if let Some(short) = out.get_mut(..T::SIZE.saturating_sub(1)) {
        if T::SIZE > 0 {
            assert!(value.write(short).is_none(), "{} wrote short", T::C_NAME);
        }
    }
}

/// Every `layout!` structure the ioctls take.
fn layouts(bytes: &[u8]) {
    layout::<drm::GemClose>(bytes);
    layout::<drm::PrimeHandle>(bytes);
    layout::<drm::GetCap>(bytes);
    layout::<drm::SetClientCap>(bytes);
    layout::<drm::ModeInfo>(bytes);
    layout::<drm::CardRes>(bytes);
    layout::<drm::Crtc>(bytes);
    layout::<drm::GetEncoder>(bytes);
    layout::<drm::GetConnector>(bytes);
    layout::<drm::FbCmd>(bytes);
    layout::<drm::FbCmd2>(bytes);
    layout::<drm::CrtcPageFlip>(bytes);
    layout::<drm::FbDirtyCmd>(bytes);
    layout::<drm::ClipRect>(bytes);
    layout::<drm::CreateDumb>(bytes);
    layout::<drm::MapDumb>(bytes);
    layout::<drm::DestroyDumb>(bytes);
    layout::<drm::GetPlaneRes>(bytes);
    layout::<drm::GetPlane>(bytes);
    layout::<drm::ObjGetProperties>(bytes);
    layout::<drm::GetProperty>(bytes);
    layout::<drm::GetBlob>(bytes);
    layout::<drm::PropertyEnum>(bytes);
    layout::<drm::Event>(bytes);
    layout::<drm::EventVblank>(bytes);
    layout::<input::InputId>(bytes);
    layout::<input::AbsInfo>(bytes);
    layout::<virtgpu::GetParam>(bytes);
    layout::<virtgpu::ContextInit>(bytes);
    layout::<virtgpu::ContextSetParam>(bytes);
    layout::<virtgpu::ResourceCreate>(bytes);
    layout::<virtgpu::ResourceInfo>(bytes);
    layout::<virtgpu::Map>(bytes);
    layout::<virtgpu::GetCaps>(bytes);
    layout::<virtgpu::ExecBuffer>(bytes);
    layout::<virtgpu::Box3d>(bytes);
    layout::<virtgpu::TransferToHost>(bytes);
    layout::<virtgpu::TransferFromHost>(bytes);
    layout::<virtgpu::Wait>(bytes);
}

/// Property 2 for the records whose layout depends on the pointer width.
fn widths(bytes: &[u8], width: Width) {
    let mut out = [0_u8; 128];

    if let Some(header) = MsgHdr::decode(bytes, width) {
        assert!(
            header.encode(&mut out, width).is_some(),
            "{header:?} did not encode"
        );
        assert_eq!(
            MsgHdr::decode(&out, width),
            Some(header),
            "{header:?} did not read back"
        );
    } else {
        assert!(
            bytes.len() < MsgHdr::size(width),
            "msghdr refused {} bytes",
            bytes.len()
        );
    }

    if let Some(header) = CmsgHdr::decode(bytes, width) {
        assert!(
            header.encode(&mut out, width).is_some(),
            "{header:?} did not encode"
        );
        assert_eq!(
            CmsgHdr::decode(&out, width),
            Some(header),
            "{header:?} did not read back"
        );
    } else {
        assert!(
            bytes.len() < CmsgHdr::size(width),
            "cmsghdr refused {} bytes",
            bytes.len()
        );
    }

    if let Some(version) = drm::Version::read(width, bytes) {
        assert!(
            version.write(width, &mut out).is_some(),
            "{version:?} did not write"
        );
        assert_eq!(
            drm::Version::read(width, &out),
            Some(version),
            "{version:?} did not read back"
        );
    } else {
        assert!(
            bytes.len() < drm::Version::size(width),
            "drm_version refused {} bytes",
            bytes.len()
        );
    }

    if let Some(event) = input::Event::read(width, bytes) {
        assert!(
            event.write(width, &mut out).is_some(),
            "{event:?} did not write"
        );
        assert_eq!(
            input::Event::read(width, &out),
            Some(event),
            "{event:?} did not read back"
        );
    } else {
        assert!(
            bytes.len() < input::Event::size(width),
            "input_event refused {} bytes",
            bytes.len()
        );
    }
}

/// Property 4: walk the bytes as a `msg_control` buffer.
fn control_messages(bytes: &[u8], width: Width) {
    let ceiling = bytes.len() / CmsgHdr::size(width) + 1;
    let mut walker = ControlMessages::new(bytes, width);
    let mut seen = 0;
    while let Some(message) = walker.next() {
        seen += 1;
        assert!(
            seen <= ceiling,
            "a walk over {} bytes yielded {seen} messages",
            bytes.len()
        );
        let Ok(message) = message else {
            assert!(walker.next().is_none(), "a walk went on after a refusal");
            return;
        };
        assert!(
            inside(bytes, message.data),
            "a message's data came from outside"
        );
    }
}

/// Property 5.
fn numbers(bytes: &[u8]) {
    for chunk in bytes.chunks(4) {
        let mut word = [0_u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        let word = u32::from_le_bytes(word);
        for number in [word as usize, (word & 0x7ff) as usize] {
            let _ = nr::from_x86_64(number);
            let _ = nr::from_aarch64(number);
            let _ = nr::from_arm(number);
        }

        let (dir, number, size) = (word >> 30, word & 0xff, (word >> 8) & 0x3fff);
        let request = input::ioc(dir, number, size);
        assert_eq!(input::ioc_dir(request), dir, "direction of {request:#x}");
        assert_eq!(
            input::ioc_type(request),
            input::IOC_TYPE_EVDEV,
            "type of {request:#x}"
        );
        assert_eq!(input::ioc_nr(request), number, "number of {request:#x}");
        assert_eq!(input::ioc_size(request), size, "size of {request:#x}");
    }

    let mut words = bytes.chunks(8).map(|chunk| {
        let mut word = [0_u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        u64::from_le_bytes(word)
    });
    let mut next = || words.next().unwrap_or(0);
    let (a, b, c) = (next(), next(), next());
    let aarch64 = hwcap::aarch64::IdRegisters {
        pfr0: a,
        isar0: b,
        isar1: c,
    };
    let _ = hwcap::aarch64::hwcap(aarch64);
    let _ = hwcap::aarch64::hwcap2(aarch64);
    let arm = hwcap::arm::IdRegisters {
        id_isar0: a as u32,
        id_mmfr0: (a >> 32) as u32,
        mvfr0: b as u32,
        mvfr1: (b >> 32) as u32,
    };
    let _ = hwcap::arm::hwcap(arm);
}

fuzz_target!(|data: &[u8]| {
    // The first byte chooses the width and a claimed address length; the
    // rest is the memory the program pointed at.
    let Some((&control, bytes)) = data.split_first() else {
        return;
    };
    let width = if control & 1 == 0 {
        Width::Bits64
    } else {
        Width::Bits32
    };

    addresses(bytes, usize::from(control >> 1));
    addresses(bytes, bytes.len());
    fixed_records!(bytes; Ucred, Linger, NlMsgHdr, NlMsgErr, NlAttr, IfInfoMsg, IfAddrMsg, RtMsg, NdMsg);
    layouts(bytes);
    widths(bytes, width);
    control_messages(bytes, width);
    numbers(bytes);
});
