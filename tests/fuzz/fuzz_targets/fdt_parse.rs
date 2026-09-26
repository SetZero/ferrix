//! Fuzz the flattened device tree reader.
//!
//! On ARMv7-A the device tree is the only description of the machine, and the
//! kernel reads its console, interrupt controller, timer and PSCI conduit out
//! of it before it has printed a line; from stage 10 every architecture reads
//! PCI host bridges, SMMUs and `GICv2m` frames from it too. The blob is
//! whatever firmware left, and it is parsed in ring 0 with `overflow-checks`
//! on.
//!
//! # The properties
//!
//! Not panicking is the floor. Beyond it:
//!
//! 1. **A tree `Fdt::parse` accepts is well formed by the specification.** A
//!    second walk of the token stream, written here from the format and not
//!    from the crate, succeeds on it and ends with every node closed.
//! 2. **The walk is that walk**: `nodes()` yields exactly the nodes the
//!    second walk found, in order, at the same depths, with the cell counts
//!    their parents declared, and each node's `properties()` are the ones
//!    that follow its name, byte for byte.
//! 3. **Everything borrowed lies inside the blob's `totalsize`**, and the
//!    same tree read from the blob cut at `totalsize` is the same tree.
//! 4. **`find_node` finds the first node at a path**, as the specification's
//!    matching rule says, for every node whose path can be written.
//! 5. **Every accessor's walk ends within a bound the blob sets**, and what
//!    each reports is internally consistent: a timer interrupt is one a GIC
//!    delivers, an ECAM host's buses fit its window, an `iommu-map`
//!    translation lands inside the entry that produced it, a processor is a
//!    direct child of `/cpus`.

#![no_main]

use ferrix_fdt::{
    DEFAULT_ADDRESS_CELLS, DEFAULT_SIZE_CELLS, FDT_BEGIN_NODE, FDT_END, FDT_END_NODE, FDT_NOP,
    FDT_PROP, Fdt, MAX_CELLS, Node, PSCI_COMPATIBLES, Property, TimerInterrupt,
    VIRTIO_MMIO_COMPATIBLE,
};
use libfuzzer_sys::fuzz_target;

/// The most nodes the quadratic checks look at, so a large tree spends its
/// time on the linear ones rather than on path lookups.
const QUADRATIC_NODES: usize = 128;

/// One node as the second walk sees it.
#[derive(Debug)]
struct Expected<'a> {
    name: &'a str,
    depth: usize,
    path: Vec<&'a str>,
    address_cells: u32,
    size_cells: u32,
    properties: Vec<Property<'a>>,
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn align4(value: usize) -> usize {
    (value + 3) & !3
}

/// The NUL-terminated UTF-8 string at `at`, which a tree that parsed must have.
fn cstr(bytes: &[u8], at: usize) -> &str {
    let tail = &bytes[at..];
    let end = tail
        .iter()
        .position(|&byte| byte == 0)
        .expect("parse accepted an unterminated name");
    core::str::from_utf8(&tail[..end]).expect("parse accepted a name that is not UTF-8")
}

/// Whether `inner` lies inside `outer`; an empty slice reads nothing.
fn inside(outer: &[u8], inner: &[u8]) -> bool {
    let (outer, inner_range) = (outer.as_ptr_range(), inner.as_ptr_range());
    inner.is_empty() || (outer.start <= inner_range.start && inner_range.end <= outer.end)
}

/// The property whose `FDT_PROP` token was just read, and where the stream
/// continues.
fn property<'a>(structs: &'a [u8], strings: &'a [u8], at: usize) -> (Property<'a>, usize) {
    let len = be32(structs, at).expect("a property's length") as usize;
    let nameoff = be32(structs, at + 4).expect("a property's name offset") as usize;
    let value = &structs[at + 8..at + 8 + len];
    let name = cstr(strings, nameoff);
    (Property { name, value }, align4(at + 8 + len))
}

/// Property 1: the specification's walk, over a tree the crate accepted.
fn second_walk<'a>(structs: &'a [u8], strings: &'a [u8]) -> Vec<Expected<'a>> {
    let mut nodes = Vec::new();
    // Per open node: its name and the cell counts it declares for children.
    let mut open: Vec<(&str, u32, u32)> = Vec::new();
    let mut at = 0;
    loop {
        let token = be32(structs, at).expect("parse accepted a stream with no end");
        at += 4;
        match token {
            FDT_BEGIN_NODE => {
                let name = cstr(structs, at);
                at = align4(at + name.len() + 1);
                let (address_cells, size_cells) = open
                    .last()
                    .map_or((DEFAULT_ADDRESS_CELLS, DEFAULT_SIZE_CELLS), |parent| {
                        (parent.1, parent.2)
                    });
                let mut declared = (address_cells, size_cells);
                let mut properties = Vec::new();
                loop {
                    match be32(structs, at) {
                        Some(FDT_NOP) => at += 4,
                        Some(FDT_PROP) => {
                            let (found, next) = property(structs, strings, at + 4);
                            at = next;
                            let small = <[u8; 4]>::try_from(found.value)
                                .map(u32::from_be_bytes)
                                .ok()
                                .filter(|cells| *cells <= MAX_CELLS);
                            match (found.name, small) {
                                ("#address-cells", Some(cells)) => declared.0 = cells,
                                ("#size-cells", Some(cells)) => declared.1 = cells,
                                _ => {}
                            }
                            properties.push(found);
                        }
                        _ => break,
                    }
                }
                let mut path: Vec<&str> = open.iter().skip(1).map(|node| node.0).collect();
                if !open.is_empty() {
                    path.push(name);
                }
                nodes.push(Expected {
                    name,
                    depth: open.len(),
                    path,
                    address_cells,
                    size_cells,
                    properties,
                });
                open.push((name, declared.0, declared.1));
            }
            FDT_END_NODE => {
                let _ = open.pop().expect("parse accepted an end with no node open");
            }
            // A property after a subnode belongs to no node's list.
            FDT_PROP => at = property(structs, strings, at).1,
            FDT_NOP => {}
            FDT_END => {
                assert!(open.is_empty(), "parse accepted a tree left open");
                return nodes;
            }
            other => panic!("parse accepted token {other:#x}"),
        }
    }
}

fn base_name(name: &str) -> &str {
    name.split('@').next().unwrap_or(name)
}

/// Property 4: the index of the first node `find_node(path)` should return.
fn first_at(expected: &[Expected<'_>], path: &[&str]) -> Option<usize> {
    expected.iter().position(|node| {
        node.path.len() == path.len()
            && node.path.iter().zip(path).all(|(have, want)| {
                have == want || (!want.contains('@') && base_name(have) == *want)
            })
    })
}

/// Which expected node `node` is, by where its name sits in the blob.
fn index_of(expected: &[Expected<'_>], node: &Node<'_>) -> usize {
    expected
        .iter()
        .position(|candidate| core::ptr::eq(candidate.name.as_ptr(), node.name.as_ptr()))
        .expect("a node the second walk did not find")
}

fn check_tree(fdt: &Fdt<'_>, expected: &[Expected<'_>], structs: &[u8], strings: &[u8]) {
    // 2. The walk is the second walk.
    let nodes: Vec<Node<'_>> = fdt.nodes().collect();
    assert_eq!(nodes.len(), expected.len(), "the node walk stopped early");
    for (node, want) in nodes.iter().zip(expected) {
        assert!(core::ptr::eq(node.name.as_ptr(), want.name.as_ptr()));
        assert_eq!(node.name, want.name);
        assert_eq!(node.depth, want.depth, "{} at the wrong depth", node.name);
        assert_eq!(
            (node.address_cells, node.size_cells),
            (want.address_cells, want.size_cells),
            "{} decodes reg with the wrong cell counts",
            node.name
        );
        let properties: Vec<Property<'_>> = node.properties().collect();
        assert_eq!(properties, want.properties, "{}'s properties", node.name);

        // 3. Borrowed from the blocks the header names.
        assert!(inside(structs, node.name.as_bytes()));
        for found in &properties {
            assert!(inside(structs, found.value) && inside(strings, found.name.as_bytes()));
            assert_eq!(node.property(found.name).map(|p| p.name), Some(found.name));
            assert_eq!(found.cells().count(), found.len() / 4);
            let mut listed = 0;
            for text in found.strings() {
                assert!(inside(found.value, text.as_bytes()));
                listed += 1;
            }
            assert!(listed <= found.len());
            let _ = (found.as_u32(), found.as_u64(), found.as_str());
        }

        // 5. Per-node accessors.
        let width = (node.address_cells + node.size_cells) as usize * 4;
        let regions = node.reg().count();
        let reg_len = node.property("reg").map_or(0, |reg| reg.len());
        if width == 0 {
            assert_eq!(regions, 0, "zero-width cells yielded a region");
        } else {
            assert!(regions <= reg_len / width, "more regions than reg holds");
            if node.address_cells <= 2 && node.size_cells <= 2 {
                assert_eq!(regions, reg_len / width, "a region went missing");
            }
        }
        for interrupt in node.gic_interrupts().take(1024) {
            assert!((16..1020).contains(&interrupt.id));
        }
        let _ = (
            node.compatible(),
            node.device_type(),
            node.base_name(),
            node.unit_address(),
            node.is_enabled(),
            node.interrupts().map(Iterator::count),
        );
    }

    // 4. Path lookup, against the specification's matching rule.
    assert_eq!(
        fdt.find_node("/").map(|node| node.name.as_ptr()),
        nodes.first().map(|node| node.name.as_ptr()),
        "/ is not the root"
    );
    for (index, want) in expected.iter().enumerate().take(QUADRATIC_NODES) {
        if want
            .path
            .iter()
            .any(|part| part.is_empty() || part.contains('/'))
        {
            continue;
        }
        let path = format!("/{}", want.path.join("/"));
        let found = fdt
            .find_node(&path)
            .unwrap_or_else(|| panic!("{path} names a node and finds none"));
        let first = first_at(expected, &want.path).expect("the node itself matches");
        assert!(first <= index);
        assert_eq!(
            index_of(expected, &found),
            first,
            "{path} found the wrong node"
        );
        if let Some(phandle) = nodes[index].phandle() {
            let by_handle = fdt.node_by_phandle(phandle).expect("a phandle resolves");
            assert!(index_of(expected, &by_handle) <= index);
            assert_eq!(by_handle.phandle(), Some(phandle));
        }
    }
}

fn check_accessors(fdt: &Fdt<'_>, expected: &[Expected<'_>]) {
    let nodes = expected.len();
    let header = fdt.header();

    let reservations = fdt.reservations().count();
    let room = (header.totalsize - header.off_mem_rsvmap) as usize / 16;
    assert!(reservations <= room, "reservations ran past the blob");

    let memory_nodes: usize = fdt
        .nodes()
        .filter(|node| {
            node.depth == 1
                && (node.base_name() == "memory" || node.device_type() == Some("memory"))
        })
        .map(|node| node.reg().count())
        .sum();
    assert_eq!(fdt.memory().count(), memory_nodes, "memory regions");

    let mut cpus = 0;
    for cpu in fdt.cpus() {
        assert_eq!(
            cpu.node.depth, 2,
            "a processor that is not a child of /cpus"
        );
        assert_eq!(
            cpu.node.reg().next().map(|region| region.address),
            Some(cpu.id)
        );
        let _ = (cpu.enable_method(), cpu.status());
        cpus += 1;
    }
    assert!(cpus <= nodes);

    if let Some(controller) = fdt.interrupt_controller() {
        let _ = (
            controller.version,
            controller.interrupt_cells(),
            controller.distributor(),
            controller.redistributor(),
            controller.cpu_interface(),
        );
    }
    let _ = fdt.timer();
    for which in [
        TimerInterrupt::SecurePhysical,
        TimerInterrupt::NonSecurePhysical,
        TimerInterrupt::Virtual,
        TimerInterrupt::Hypervisor,
    ] {
        if let Some(id) = fdt.timer_interrupt(which) {
            assert!((16..1020).contains(&id), "timer interrupt {id}");
        }
    }
    let _ = (
        fdt.psci_conduit(),
        fdt.model(),
        fdt.bootargs(),
        fdt.stdout_path(),
        fdt.console(),
        fdt.alias("serial0"),
        fdt.boot_cpu(),
        fdt.root(),
        fdt.find_compatible(PSCI_COMPATIBLES[0]),
    );

    for host in fdt.ecam_hosts().take(nodes) {
        assert!(host.window.size >= 1 << 20, "a host smaller than a bus");
        assert!(
            host.start_bus <= host.end_bus,
            "a host whose buses run backwards"
        );
        let buses = u64::from(host.end_bus - host.start_bus) + 1;
        assert!(buses << 20 <= host.window.size, "buses past the window");
        if let Some(map) = fdt.ecam_iommu_map(&host) {
            for entry in map.entries().take(64) {
                for rid in [
                    entry.rid_base,
                    entry.rid_base.wrapping_add(entry.length / 2),
                ] {
                    // Whatever the mask did to the ID, the answer came from an
                    // entry of this map.
                    if let Some((phandle, id)) = map.translate(rid) {
                        assert!(
                            map.entries().any(|candidate| candidate.phandle == phandle
                                && id.wrapping_sub(candidate.id_base) < candidate.length),
                            "a translation no entry produced"
                        );
                    }
                    if let Some(id) = entry.translate(rid) {
                        let offset = id - entry.id_base;
                        assert!(offset < entry.length, "a translation past its entry");
                    }
                }
            }
        }
    }
    for smmu in fdt.smmu_v3s().take(nodes) {
        let _ = (smmu.region, smmu.phandle, smmu.iommu_cells);
    }
    for frame in fdt.gicv2m_frames().take(nodes) {
        let _ = (frame.region, frame.spi_base, frame.spi_count);
    }
    for device in fdt.compatible_nodes(VIRTIO_MMIO_COMPATIBLE).take(nodes) {
        assert!(device.is_enabled() && device.is_compatible(VIRTIO_MMIO_COMPATIBLE));
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(fdt) = Fdt::parse(data) else {
        return;
    };
    let header = *fdt.header();
    let blob = fdt.blob();
    assert_eq!(
        blob.len(),
        header.totalsize as usize,
        "the blob is not totalsize"
    );
    assert!(inside(data, blob));
    let start = header.off_dt_struct as usize;
    let structs = &blob[start..start + header.size_dt_struct as usize];
    let start = header.off_dt_strings as usize;
    let strings = &blob[start..start + header.size_dt_strings as usize];

    let expected = second_walk(structs, strings);
    check_tree(&fdt, &expected, structs, strings);
    check_accessors(&fdt, &expected);

    // 3. Nothing past totalsize was read: the tree cut there is the same.
    let cut = Fdt::parse(blob).expect("the blob cut at totalsize parses");
    assert!(
        fdt.nodes()
            .map(|node| (node.name, node.depth))
            .eq(cut.nodes().map(|node| (node.name, node.depth))),
        "the tree changed when cut at totalsize"
    );
});
