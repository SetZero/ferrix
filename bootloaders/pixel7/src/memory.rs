//! The phone's memory, as its device tree describes it, and the loader's
//! allocations out of it.
//!
//! UEFI hands `boot/` a memory map and an allocator; ABL hands over neither,
//! only a device tree. So the map is built here, in layers: RAM from the
//! `/memory` nodes, then everything the tree reserves -- the memory
//! reservation block and every `/reserved-memory` child with a `reg` -- then
//! the loader's own image and ABL's copy of the tree, then each allocation.
//! A later layer wins where two overlap, and only what some `/memory` node
//! calls RAM is described at all.
//!
//! Every layer is kept, and the map is computed from them when asked, rather
//! than kept as a list and split in place: the layers are few, the arithmetic
//! is a sweep over their edges, and no layer can be half-applied.

use ferrix_bootinfo::{MemKind, MemRegion, PAGE_SIZE};
use ferrix_fdt::Fdt;

/// Layers the map can hold. The Pixel 7's tree reserves about seventy ranges.
const MAX_LAYERS: usize = 192;

/// Regions a computed map can hold.
pub(crate) const MAX_REGIONS: usize = 2 * MAX_LAYERS;

/// One layer: a range, and either "this is RAM" or what it is used for.
#[derive(Clone, Copy, Debug)]
struct Layer {
    base: u64,
    end: u64,
    /// `None` for a range of RAM; otherwise what the range is.
    kind: Option<MemKind>,
}

/// An empty region, for filling arrays before they are written.
pub(crate) const NO_REGION: MemRegion = MemRegion {
    base: 0,
    len: 0,
    kind: MemKind::Reserved,
    reserved: 0,
};

/// The layers, in the order they were added.
#[derive(Debug)]
pub(crate) struct Memory {
    layers: [Layer; MAX_LAYERS],
    count: usize,
}

impl Memory {
    /// A map with nothing in it.
    pub(crate) const fn new() -> Memory {
        Memory {
            layers: [Layer {
                base: 0,
                end: 0,
                kind: None,
            }; MAX_LAYERS],
            count: 0,
        }
    }

    /// Add a layer, ignoring an empty one.
    fn push(&mut self, base: u64, end: u64, kind: Option<MemKind>) -> Result<(), &'static str> {
        if end <= base {
            return Ok(());
        }
        let slot = self
            .layers
            .get_mut(self.count)
            .ok_or("the device tree describes more memory ranges than the loader can hold")?;
        *slot = Layer { base, end, kind };
        self.count += 1;
        Ok(())
    }

    /// Record a range of RAM, rounded inwards to whole pages.
    pub(crate) fn add_ram(&mut self, base: u64, len: u64) -> Result<(), &'static str> {
        let start = base.next_multiple_of(PAGE_SIZE);
        let end = base.saturating_add(len) & !(PAGE_SIZE - 1);
        self.push(start, end, None)
    }

    /// Mark a range as `kind`, rounded outwards to whole pages.
    pub(crate) fn mark(&mut self, base: u64, len: u64, kind: MemKind) -> Result<(), &'static str> {
        let start = base & !(PAGE_SIZE - 1);
        let end = base.saturating_add(len).next_multiple_of(PAGE_SIZE);
        self.push(start, end, Some(kind))
    }

    /// Take RAM and every reservation from the device tree.
    pub(crate) fn add_device_tree(&mut self, tree: &Fdt<'_>) -> Result<(), &'static str> {
        for region in tree.memory() {
            self.add_ram(region.address, region.size)?;
        }
        for reservation in tree.reservations() {
            self.mark(reservation.address, reservation.size, MemKind::Reserved)?;
        }
        // Every child of `/reserved-memory` that has a place of its own, `no-map`
        // or not: a region the tree sets aside for a device or the secure world
        // is not the kernel's to hand out, whichever way Linux would have used
        // it. A child with only a size and an allocation range was never given
        // an address, so there is nothing to keep out.
        let mut inside = false;
        for node in tree.nodes() {
            if node.depth == 1 {
                inside = node.name == "reserved-memory";
                continue;
            }
            if inside && node.depth == 2 {
                for region in node.reg() {
                    self.mark(region.address, region.size, MemKind::Reserved)?;
                }
            }
        }
        Ok(())
    }

    /// The map as the kernel will be told it: sorted, not overlapping, only
    /// what is RAM, adjacent ranges of one kind merged. Returns how many of
    /// `out` it filled.
    pub(crate) fn regions(&self, out: &mut [MemRegion]) -> usize {
        let mut edges = [0u64; 2 * MAX_LAYERS];
        let mut edge_count = 0;
        for layer in self.layers.iter().take(self.count) {
            for edge in [layer.base, layer.end] {
                if let Some(slot) = edges.get_mut(edge_count) {
                    *slot = edge;
                    edge_count += 1;
                }
            }
        }
        let Some(edges) = edges.get_mut(..edge_count) else {
            return 0;
        };
        edges.sort_unstable();

        let mut written = 0;
        for pair in edges.windows(2) {
            let &[start, end] = pair else { continue };
            if start == end {
                continue;
            }
            let Some(kind) = self.kind_of(start, end) else {
                continue;
            };
            written = append(out, written, start, end, kind);
        }
        written
    }

    /// What `start..end`, which lies between two edges, is: `None` if no layer
    /// calls it RAM, otherwise the last layer's kind that covers it, or usable.
    fn kind_of(&self, start: u64, end: u64) -> Option<MemKind> {
        let covering = |layer: &&Layer| layer.base <= start && end <= layer.end;
        let layers = self.layers.iter().take(self.count);
        if !layers
            .clone()
            .filter(covering)
            .any(|layer| layer.kind.is_none())
        {
            return None;
        }
        Some(
            layers
                .filter(covering)
                .filter_map(|layer| layer.kind)
                .next_back()
                .unwrap_or(MemKind::Usable),
        )
    }

    /// Take `len` bytes, page aligned, from the top of the highest usable
    /// range they fit in, mark them `kind`, and return their address.
    ///
    /// Highest first keeps the loader's allocations out of the low RAM around
    /// its own image and ABL's buffers, which it has no reason to crowd.
    pub(crate) fn allocate(&mut self, len: u64, kind: MemKind) -> Result<u64, &'static str> {
        let len = len.next_multiple_of(PAGE_SIZE);
        let mut map = [NO_REGION; MAX_REGIONS];
        let count = self.regions(&mut map);
        let base = map
            .iter()
            .take(count)
            .filter(|region| region.kind == MemKind::Usable && region.len >= len)
            .map(|region| region.base + region.len - len)
            .max()
            .ok_or("no usable range of RAM is large enough for an allocation")?;
        self.mark(base, len, kind)?;
        Ok(base)
    }
}

/// Add `start..end` of `kind` to `out` after its first `written` entries,
/// extending the last one if it is the same kind and touches it.
fn append(out: &mut [MemRegion], written: usize, start: u64, end: u64, kind: MemKind) -> usize {
    if let Some(last) = written.checked_sub(1).and_then(|index| out.get_mut(index))
        && last.kind == kind
        && last.base + last.len == start
    {
        last.len = end - last.base;
        return written;
    }
    match out.get_mut(written) {
        Some(slot) => {
            *slot = MemRegion {
                base: start,
                len: end - start,
                kind,
                reserved: 0,
            };
            written + 1
        }
        None => written,
    }
}
