//! `virtgpu`: every number and layout against the probe's output, at both
//! widths, and each structure read and written back byte for byte.

use super::std::borrow::ToOwned;
use super::std::collections::BTreeMap;
use super::std::format;
use super::std::string::String;
use super::std::vec;
use super::std::vec::Vec;

use crate::virtgpu::{self, Field, Layout};

const PROBE_64: &str = include_str!("../../probe/virtgpu-64.txt");
const PROBE_32: &str = include_str!("../../probe/virtgpu-32.txt");

/// The probe's lines as `name → value`.
fn probe(text: &str) -> BTreeMap<String, u64> {
    let mut lines = BTreeMap::new();
    for line in text.lines() {
        let (name, value) = line.split_once(' ').expect("each line is `name value`");
        let value = value.parse().expect("each value is decimal");
        assert!(
            lines.insert(name.to_owned(), value).is_none(),
            "{name} printed twice"
        );
    }
    lines
}

/// The ioctl numbers.
fn ioctls() -> Vec<(&'static str, u64)> {
    vec![
        ("DRM_IOCTL_VIRTGPU_MAP", virtgpu::IOCTL_MAP.into()),
        (
            "DRM_IOCTL_VIRTGPU_EXECBUFFER",
            virtgpu::IOCTL_EXECBUFFER.into(),
        ),
        ("DRM_IOCTL_VIRTGPU_GETPARAM", virtgpu::IOCTL_GETPARAM.into()),
        (
            "DRM_IOCTL_VIRTGPU_RESOURCE_CREATE",
            virtgpu::IOCTL_RESOURCE_CREATE.into(),
        ),
        (
            "DRM_IOCTL_VIRTGPU_RESOURCE_INFO",
            virtgpu::IOCTL_RESOURCE_INFO.into(),
        ),
        (
            "DRM_IOCTL_VIRTGPU_TRANSFER_FROM_HOST",
            virtgpu::IOCTL_TRANSFER_FROM_HOST.into(),
        ),
        (
            "DRM_IOCTL_VIRTGPU_TRANSFER_TO_HOST",
            virtgpu::IOCTL_TRANSFER_TO_HOST.into(),
        ),
        ("DRM_IOCTL_VIRTGPU_WAIT", virtgpu::IOCTL_WAIT.into()),
        ("DRM_IOCTL_VIRTGPU_GET_CAPS", virtgpu::IOCTL_GET_CAPS.into()),
        (
            "DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB",
            virtgpu::IOCTL_RESOURCE_CREATE_BLOB.into(),
        ),
        (
            "DRM_IOCTL_VIRTGPU_CONTEXT_INIT",
            virtgpu::IOCTL_CONTEXT_INIT.into(),
        ),
    ]
}

/// Every other constant.
fn values() -> Vec<(&'static str, u64)> {
    vec![
        ("VIRTGPU_PARAM_3D_FEATURES", virtgpu::PARAM_3D_FEATURES),
        (
            "VIRTGPU_PARAM_CAPSET_QUERY_FIX",
            virtgpu::PARAM_CAPSET_QUERY_FIX,
        ),
        ("VIRTGPU_PARAM_RESOURCE_BLOB", virtgpu::PARAM_RESOURCE_BLOB),
        ("VIRTGPU_PARAM_HOST_VISIBLE", virtgpu::PARAM_HOST_VISIBLE),
        ("VIRTGPU_PARAM_CROSS_DEVICE", virtgpu::PARAM_CROSS_DEVICE),
        ("VIRTGPU_PARAM_CONTEXT_INIT", virtgpu::PARAM_CONTEXT_INIT),
        // The header really does spell this one with a lowercase `s`.
        (
            "VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs",
            virtgpu::PARAM_SUPPORTED_CAPSET_IDS,
        ),
        (
            "VIRTGPU_PARAM_EXPLICIT_DEBUG_NAME",
            virtgpu::PARAM_EXPLICIT_DEBUG_NAME,
        ),
        ("VIRTGPU_DRM_CAPSET_VIRGL", virtgpu::CAPSET_VIRGL.into()),
        ("VIRTGPU_DRM_CAPSET_VIRGL2", virtgpu::CAPSET_VIRGL2.into()),
        ("VIRTGPU_DRM_CAPSET_VENUS", virtgpu::CAPSET_VENUS.into()),
        ("VIRTGPU_BLOB_MEM_GUEST", virtgpu::BLOB_MEM_GUEST.into()),
        ("VIRTGPU_BLOB_MEM_HOST3D", virtgpu::BLOB_MEM_HOST3D.into()),
        (
            "VIRTGPU_BLOB_MEM_HOST3D_GUEST",
            virtgpu::BLOB_MEM_HOST3D_GUEST.into(),
        ),
        (
            "VIRTGPU_BLOB_FLAG_USE_MAPPABLE",
            virtgpu::BLOB_FLAG_USE_MAPPABLE.into(),
        ),
        (
            "VIRTGPU_BLOB_FLAG_USE_SHAREABLE",
            virtgpu::BLOB_FLAG_USE_SHAREABLE.into(),
        ),
        (
            "VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE",
            virtgpu::BLOB_FLAG_USE_CROSS_DEVICE.into(),
        ),
        (
            "VIRTGPU_CONTEXT_PARAM_CAPSET_ID",
            virtgpu::CONTEXT_PARAM_CAPSET_ID,
        ),
        (
            "VIRTGPU_CONTEXT_PARAM_NUM_RINGS",
            virtgpu::CONTEXT_PARAM_NUM_RINGS,
        ),
        (
            "VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK",
            virtgpu::CONTEXT_PARAM_POLL_RINGS_MASK,
        ),
        (
            "VIRTGPU_CONTEXT_PARAM_DEBUG_NAME",
            virtgpu::CONTEXT_PARAM_DEBUG_NAME,
        ),
        (
            "VIRTGPU_EXECBUF_FENCE_FD_IN",
            virtgpu::EXECBUF_FENCE_FD_IN.into(),
        ),
        (
            "VIRTGPU_EXECBUF_FENCE_FD_OUT",
            virtgpu::EXECBUF_FENCE_FD_OUT.into(),
        ),
        ("VIRTGPU_EXECBUF_RING_IDX", virtgpu::EXECBUF_RING_IDX.into()),
        ("VIRTGPU_WAIT_NOWAIT", virtgpu::WAIT_NOWAIT.into()),
    ]
}

/// A layout's `sizeof` and `offsetof` lines as the probe prints them.
fn layout_lines(c_name: &str, size: usize, fields: &[(&str, usize)]) -> Vec<(String, u64)> {
    let mut lines = vec![(format!("sizeof.{c_name}"), size as u64)];
    for (field, offset) in fields {
        lines.push((format!("offsetof.{c_name}.{field}"), *offset as u64));
    }
    lines
}

fn layouts<L: Layout>() -> Vec<(String, u64)> {
    layout_lines(L::C_NAME, L::SIZE, L::FIELDS)
}

/// Every line this module says the probe prints.
fn expected() -> BTreeMap<String, u64> {
    let mut lines: Vec<(String, u64)> = ioctls()
        .into_iter()
        .chain(values())
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
    lines.extend(layouts::<virtgpu::GetParam>());
    lines.extend(layouts::<virtgpu::ContextInit>());
    lines.extend(layouts::<virtgpu::ContextSetParam>());
    lines.extend(layouts::<virtgpu::ResourceCreate>());
    lines.extend(layouts::<virtgpu::ResourceCreateBlob>());
    lines.extend(layouts::<virtgpu::ResourceInfo>());
    lines.extend(layouts::<virtgpu::Map>());
    lines.extend(layouts::<virtgpu::GetCaps>());
    lines.extend(layouts::<virtgpu::ExecBuffer>());
    lines.extend(layouts::<virtgpu::Box3d>());
    lines.extend(layouts::<virtgpu::TransferToHost>());
    lines.extend(layouts::<virtgpu::TransferFromHost>());
    lines.extend(layouts::<virtgpu::Wait>());
    let count = lines.len();
    let map: BTreeMap<String, u64> = lines.into_iter().collect();
    assert_eq!(map.len(), count, "a line is defined twice");
    map
}

/// Every line on which this module and the probe disagree, one per line.
fn disagreements(text: &str) -> Vec<String> {
    let ours = expected();
    let theirs = probe(text);
    let mut names: Vec<&String> = ours.keys().chain(theirs.keys()).collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter(|name| ours.get(*name) != theirs.get(*name))
        .map(|name| {
            format!(
                "{name}: this module says {:?}, the probe printed {:?}",
                ours.get(name),
                theirs.get(name)
            )
        })
        .collect()
}

#[test]
fn every_number_and_layout_matches_the_probe_at_64_bits() {
    assert_eq!(disagreements(PROBE_64), Vec::<String>::new());
}

#[test]
fn every_number_and_layout_matches_the_probe_at_32_bits() {
    assert_eq!(disagreements(PROBE_32), Vec::<String>::new());
}

/// Nothing here has a width: the render node's structures carry their user
/// pointers as `__u64`, so both probes print the same bytes. A field that
/// grew a `size_t` or a `char *` would show up here first.
#[test]
fn nothing_differs_between_the_widths() {
    assert_eq!(probe(PROBE_64), probe(PROBE_32));
}

/// Bytes counting up from `seed`, so every field reads a different value.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|index| seed.wrapping_add((index % 251) as u8))
        .collect()
}

fn round_trip<L: Layout + core::fmt::Debug + PartialEq>() {
    let bytes = pattern(L::SIZE, 7);
    let value = L::read(&bytes).expect("a whole structure reads");
    let mut out = vec![0u8; L::SIZE];
    value.write(&mut out).expect("it fits");

    // Every byte comes back as it was: no structure here has padding a field
    // does not cover, so nothing may be dropped.
    assert_eq!(out, bytes, "{} did not come back whole", L::C_NAME);
    assert_eq!(L::read(&out), Some(value));
    assert_eq!(L::read(&bytes[..L::SIZE - 1]), None, "{} short", L::C_NAME);
    assert_eq!(value.write(&mut out[..L::SIZE - 1]), None);
    assert_eq!(<L as Field>::get(&bytes, 0), Some(value));
    assert_eq!(<L as Field>::get(&bytes, 1), None);
}

#[test]
fn every_structure_reads_and_writes_back() {
    round_trip::<virtgpu::GetParam>();
    round_trip::<virtgpu::ContextInit>();
    round_trip::<virtgpu::ContextSetParam>();
    round_trip::<virtgpu::ResourceCreate>();
    round_trip::<virtgpu::ResourceCreateBlob>();
    round_trip::<virtgpu::ResourceInfo>();
    round_trip::<virtgpu::Map>();
    round_trip::<virtgpu::GetCaps>();
    round_trip::<virtgpu::ExecBuffer>();
    round_trip::<virtgpu::Box3d>();
    round_trip::<virtgpu::TransferToHost>();
    round_trip::<virtgpu::TransferFromHost>();
    round_trip::<virtgpu::Wait>();
}

/// The two transfers are one layout under two names, which is what lets a
/// kernel read either with one reader: written as one and read as the
/// other, every field comes back.
#[test]
fn the_two_transfers_are_one_layout() {
    assert_eq!(
        virtgpu::TransferToHost::SIZE,
        virtgpu::TransferFromHost::SIZE
    );
    let mut bytes = vec![0u8; virtgpu::TransferToHost::SIZE];
    virtgpu::TransferToHost {
        bo_handle: 1,
        r#box: virtgpu::Box3d {
            x: 2,
            y: 3,
            z: 4,
            w: 5,
            h: 6,
            d: 7,
        },
        level: 8,
        offset: 9,
        stride: 10,
        layer_stride: 11,
    }
    .write(&mut bytes)
    .expect("it fits");
    let other = virtgpu::TransferFromHost::read(&bytes).expect("it reads");
    assert_eq!(other.bo_handle, 1);
    assert_eq!((other.r#box.x, other.r#box.y, other.r#box.z), (2, 3, 4));
    assert_eq!((other.r#box.w, other.r#box.h, other.r#box.d), (5, 6, 7));
    assert_eq!(
        (other.level, other.offset, other.stride, other.layer_stride),
        (8, 9, 10, 11)
    );
}

#[test]
fn fields_land_where_the_headers_put_them() {
    // The buffer the driver already creates on the device: a 4096-byte
    // vertex buffer, `PIPE_BUFFER` of `VIRGL_FORMAT_R8_UNORM`.
    let mut bytes = vec![0u8; virtgpu::ResourceCreate::SIZE];
    virtgpu::ResourceCreate {
        target: 0,
        format: 64,
        bind: 1 << 4,
        width: 4096,
        height: 1,
        depth: 1,
        array_size: 1,
        size: 4096,
        ..virtgpu::ResourceCreate::ZERO
    }
    .write(&mut bytes)
    .expect("it fits");
    assert_eq!(bytes[4..8], 64u32.to_le_bytes());
    assert_eq!(bytes[8..12], 16u32.to_le_bytes());
    assert_eq!(bytes[12..16], 4096u32.to_le_bytes());
    assert_eq!(bytes[48..52], 4096u32.to_le_bytes());

    // A submission's command buffer is a user address in the middle of the
    // structure, and `fence_fd` is signed, so `-1` is all ones.
    let mut exec = [0u8; 64];
    virtgpu::ExecBuffer {
        flags: virtgpu::EXECBUF_RING_IDX,
        size: 128,
        command: 0x7FFF_0000_1000,
        fence_fd: -1,
        ring_idx: 0,
        ..virtgpu::ExecBuffer::ZERO
    }
    .write(&mut exec)
    .expect("it fits");
    assert_eq!(exec[0..4], 4u32.to_le_bytes());
    assert_eq!(exec[8..16], 0x7FFF_0000_1000u64.to_le_bytes());
    assert_eq!(exec[28..32], [0xFF, 0xFF, 0xFF, 0xFF]);

    // A transfer carries its box inline, not behind a pointer.
    let mut transfer = [0u8; 44];
    virtgpu::TransferToHost {
        bo_handle: 1,
        r#box: virtgpu::Box3d {
            x: 0,
            y: 0,
            z: 0,
            w: 4096,
            h: 1,
            d: 1,
        },
        level: 0,
        offset: 0,
        stride: 4096,
        layer_stride: 4096,
    }
    .write(&mut transfer)
    .expect("it fits");
    assert_eq!(transfer[0..4], 1u32.to_le_bytes());
    assert_eq!(transfer[16..20], 4096u32.to_le_bytes());
    assert_eq!(transfer[36..40], 4096u32.to_le_bytes());
}
