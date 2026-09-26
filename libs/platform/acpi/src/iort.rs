//! The IORT: how an Arm machine's devices reach memory, and through which
//! SMMU.
//!
//! On AArch64 under ACPI the `SMMUv3` an IOMMU domain is built on is found only
//! here. The table is a graph rather than a list: nodes — a root complex, an
//! SMMU, an ITS group — each carrying *ID mappings* that say which range of
//! input identifiers leaves the node as which output identifiers, towards
//! which other node, named by its offset in the table. A PCI function's
//! requester ID enters at the root complex, and following the mappings tells
//! the kernel which SMMU translates it, and under which stream ID.
//!
//! Nodes are read by offset because that is how they refer to each other, and
//! every offset and length is checked against the table before it is used. A
//! node count larger than the nodes present, or a mapping count larger than
//! the mappings present, ends the walk where the bytes do.

use crate::{AcpiError, Table, u8_at, u16_at, u32_at, u64_at};

/// The IORT's signature.
pub const IORT_SIGNATURE: [u8; 4] = *b"IORT";

/// Bytes of the IORT's fixed header: the system description header, the node
/// count, the offset of the first node and four reserved bytes.
pub const IORT_HEADER_LEN: usize = 48;

/// Node type: an ITS group.
pub const NODE_ITS_GROUP: u8 = 0;
/// Node type: a named component, a platform device.
pub const NODE_NAMED_COMPONENT: u8 = 1;
/// Node type: a PCI root complex.
pub const NODE_ROOT_COMPLEX: u8 = 2;
/// Node type: an `SMMUv1` or `SMMUv2`.
pub const NODE_SMMU_V1_V2: u8 = 3;
/// Node type: an `SMMUv3`.
pub const NODE_SMMU_V3: u8 = 4;

/// Bytes of every node's common header.
pub const NODE_HEADER_LEN: usize = 16;
/// Bytes of one ID mapping.
pub const ID_MAPPING_LEN: usize = 20;

/// ID mapping flags: the mapping is the node's own single ID, not a range.
pub const ID_MAPPING_SINGLE: u32 = 1 << 0;

/// Bytes of an `SMMUv3` node up to and including its last interrupt field.
const SMMU_V3_MIN_LEN: usize = 60;
/// Bytes of a root complex node up to and including its segment number.
const ROOT_COMPLEX_MIN_LEN: usize = 32;

/// The I/O remapping table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Iort<'a> {
    /// The table everything is read out of.
    table: Table<'a>,
}

impl<'a> Iort<'a> {
    /// Interpret `table` as an IORT.
    ///
    /// # Errors
    ///
    /// [`AcpiError::BadSignature`] if it is not one, and
    /// [`AcpiError::TooShort`] if it ends inside the fixed header.
    pub fn parse(table: Table<'a>) -> Result<Self, AcpiError> {
        table.expect_signature(IORT_SIGNATURE)?;
        if table.bytes().len() < IORT_HEADER_LEN {
            return Err(AcpiError::TooShort {
                got: table.bytes().len(),
                need: IORT_HEADER_LEN,
            });
        }
        Ok(Iort { table })
    }

    /// The underlying table, for its header and checksum.
    #[must_use]
    pub const fn table(&self) -> &Table<'a> {
        &self.table
    }

    /// How many nodes the table says it has.
    #[must_use]
    pub fn node_count(&self) -> u32 {
        u32_at(self.table.bytes(), 36).unwrap_or(0)
    }

    /// Every node, in table order, stopping at the first that does not fit.
    #[must_use]
    pub fn nodes(&self) -> Nodes<'a> {
        Nodes {
            bytes: self.table.bytes(),
            offset: u32_at(self.table.bytes(), 40).unwrap_or(0),
            remaining: self.node_count(),
        }
    }

    /// The node at `offset` into the table, which is how an ID mapping names
    /// the node its identifiers go to.
    #[must_use]
    pub fn node_at(&self, offset: u32) -> Option<Node<'a>> {
        decode_node(self.table.bytes(), offset)
    }
}

/// Decode the node at `offset` in `bytes`, if it fits.
fn decode_node(bytes: &[u8], offset: u32) -> Option<Node<'_>> {
    let start = usize::try_from(offset).ok()?;
    let head = bytes.get(start..)?;
    let length = u16_at(head, 1)?;
    let span = usize::from(length);
    if span < NODE_HEADER_LEN {
        return None;
    }
    let node = head.get(..span)?;
    Some(Node {
        kind: u8_at(node, 0)?,
        revision: u8_at(node, 3)?,
        identifier: u32_at(node, 4)?,
        offset,
        mapping_count: u32_at(node, 8)?,
        mapping_offset: u32_at(node, 12)?,
        bytes: node,
    })
}

/// Iterator over an IORT's nodes.
#[derive(Clone, Copy, Debug)]
pub struct Nodes<'a> {
    /// The whole table.
    bytes: &'a [u8],
    /// Where the next node starts.
    offset: u32,
    /// Nodes the table says are left.
    remaining: u32,
}

impl<'a> Iterator for Nodes<'a> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Node<'a>> {
        if self.remaining == 0 {
            return None;
        }
        let Some(node) = decode_node(self.bytes, self.offset) else {
            self.remaining = 0;
            return None;
        };
        self.remaining -= 1;
        // A node's length is at least its header, so this advances; a table
        // whose offsets would wrap has already failed to decode.
        self.offset = self
            .offset
            .saturating_add(u32::try_from(node.bytes.len()).unwrap_or(u32::MAX));
        Some(node)
    }
}

/// One node.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Node<'a> {
    /// [`NODE_ROOT_COMPLEX`] and its neighbours.
    pub kind: u8,
    /// The node format's revision.
    pub revision: u8,
    /// Firmware's identifier for the node, unique within the table.
    pub identifier: u32,
    /// The node's offset in the table, which is how others refer to it.
    pub offset: u32,
    /// How many ID mappings the node says it has.
    mapping_count: u32,
    /// Where they start, relative to the node.
    mapping_offset: u32,
    /// The node's own bytes, exactly its length.
    bytes: &'a [u8],
}

impl<'a> Node<'a> {
    /// The node's ID mappings, stopping at the first that does not fit.
    #[must_use]
    pub fn id_mappings(&self) -> IdMappings<'a> {
        IdMappings {
            bytes: self.bytes,
            offset: usize::try_from(self.mapping_offset).unwrap_or(usize::MAX),
            remaining: self.mapping_count,
        }
    }

    /// Where input identifier `id` leaves this node: the output identifier and
    /// the offset of the node it goes to, from the first range mapping that
    /// contains it.
    #[must_use]
    pub fn translate(&self, id: u32) -> Option<(u32, u32)> {
        self.id_mappings().find_map(|mapping| {
            mapping
                .translate(id)
                .map(|output| (output, mapping.output_reference))
        })
    }

    /// The node as an `SMMUv3`, if it is one and long enough.
    #[must_use]
    pub fn smmu_v3(&self) -> Option<SmmuV3> {
        if self.kind != NODE_SMMU_V3 || self.bytes.len() < SMMU_V3_MIN_LEN {
            return None;
        }
        Some(SmmuV3 {
            base_address: u64_at(self.bytes, 16)?,
            flags: u32_at(self.bytes, 24)?,
            model: u32_at(self.bytes, 40)?,
            event_gsiv: u32_at(self.bytes, 44)?,
            pri_gsiv: u32_at(self.bytes, 48)?,
            gerr_gsiv: u32_at(self.bytes, 52)?,
            sync_gsiv: u32_at(self.bytes, 56)?,
        })
    }

    /// The node as a PCI root complex, if it is one and long enough.
    #[must_use]
    pub fn root_complex(&self) -> Option<RootComplex> {
        if self.kind != NODE_ROOT_COMPLEX || self.bytes.len() < ROOT_COMPLEX_MIN_LEN {
            return None;
        }
        Some(RootComplex {
            ats_attribute: u32_at(self.bytes, 24)?,
            segment: u32_at(self.bytes, 28)?,
        })
    }
}

/// One ID mapping.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IdMapping {
    /// The first input identifier.
    pub input_base: u32,
    /// How many identifiers the range covers. Stored less one, returned whole,
    /// which is why it is wider than the fields around it.
    pub count: u64,
    /// The output identifier `input_base` maps to.
    pub output_base: u32,
    /// The offset of the node the identifiers go to.
    pub output_reference: u32,
    /// [`ID_MAPPING_SINGLE`] and its neighbours.
    pub flags: u32,
}

impl IdMapping {
    /// Whether this is the node's own single ID rather than a range.
    #[must_use]
    pub const fn is_single(&self) -> bool {
        self.flags & ID_MAPPING_SINGLE != 0
    }

    /// The output identifier for `id`, if the range covers it. A single
    /// mapping translates no input: it names the node's own ID.
    #[must_use]
    pub const fn translate(&self, id: u32) -> Option<u32> {
        if self.is_single() {
            return None;
        }
        let Some(offset) = id.checked_sub(self.input_base) else {
            return None;
        };
        if offset as u64 >= self.count {
            return None;
        }
        self.output_base.checked_add(offset)
    }
}

/// Iterator over a node's ID mappings.
#[derive(Clone, Copy, Debug)]
pub struct IdMappings<'a> {
    /// The node's bytes.
    bytes: &'a [u8],
    /// Where the next mapping starts, relative to the node.
    offset: usize,
    /// Mappings the node says are left.
    remaining: u32,
}

impl Iterator for IdMappings<'_> {
    type Item = IdMapping;

    fn next(&mut self) -> Option<IdMapping> {
        if self.remaining == 0 {
            return None;
        }
        let end = self.offset.checked_add(ID_MAPPING_LEN)?;
        let Some(entry) = self.bytes.get(self.offset..end) else {
            self.remaining = 0;
            return None;
        };
        self.remaining -= 1;
        self.offset = end;
        Some(IdMapping {
            input_base: u32_at(entry, 0)?,
            count: u64::from(u32_at(entry, 4)?) + 1,
            output_base: u32_at(entry, 8)?,
            output_reference: u32_at(entry, 12)?,
            flags: u32_at(entry, 16)?,
        })
    }
}

/// An `SMMUv3`'s registers and interrupts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SmmuV3 {
    /// Physical address of the register block.
    pub base_address: u64,
    /// Node flags, among them whether coherent access is overridden.
    pub flags: u32,
    /// Zero for a generic `SMMUv3`, otherwise a known implementation's quirks.
    pub model: u32,
    /// The event queue's interrupt.
    pub event_gsiv: u32,
    /// The page request queue's interrupt.
    pub pri_gsiv: u32,
    /// The global error interrupt.
    pub gerr_gsiv: u32,
    /// The command queue sync interrupt.
    pub sync_gsiv: u32,
}

/// A PCI root complex's properties.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RootComplex {
    /// Whether devices below it support ATS.
    pub ats_attribute: u32,
    /// The PCI segment group, matching the MCFG's.
    pub segment: u32,
}
