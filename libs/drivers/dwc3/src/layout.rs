//! Where everything the controller reads or writes lives in the DMA area.
//!
//! Seven pages, [`AREA_BYTES`]. Nothing crosses a page, so a pin that gives
//! the area a page at a time, wherever each page is, is all the controller
//! needs:
//!
//! * page 0: the event buffer, one page, which the controller writes and
//!   the processor only reads;
//! * page 1: the TRBs. Endpoint 0's two in one cache line, then each other
//!   endpoint's two in a line of its own: the controller writes a TRB back
//!   when it finishes one, and a line the processor was writing for another
//!   endpoint at the time would, written back from the cache, undo it;
//! * page 2: the SETUP packet, in a line of its own because the controller
//!   writes it while the processor may be writing the data buffer, and
//!   endpoint 0's data buffer;
//! * pages 3 to 6: a page for each of [`MAX_ENDPOINTS`] other endpoints --
//!   a ring of bytes waiting to go for an IN endpoint, the packet being
//!   received for an OUT one.

use crate::trb::TRB_BYTES;

/// A page.
pub const PAGE: usize = 4096;
/// The processor's cache line, for the Cortex-A55, A76 and X1 alike. Every
/// piece the controller writes starts on one and fills whole ones.
pub const CACHE_LINE: usize = 64;
/// How many endpoints besides endpoint 0 there are places for.
pub const MAX_ENDPOINTS: usize = 4;

/// The event buffer.
pub const EVENTS: usize = 0;
/// Its size: `GEVNTSIZ`.
pub const EVENT_BYTES: usize = PAGE;

/// Endpoint 0's TRBs: the one a stage uses, and the zero-length packet
/// after an IN data stage that needs one.
pub const EP0_TRBS: usize = PAGE;
/// Each other endpoint's TRBs: data, and a zero-length packet.
#[must_use]
pub const fn endpoint_trbs(slot: usize) -> usize {
    PAGE + CACHE_LINE * (1 + slot)
}
/// The TRBs each endpoint has.
pub const TRBS_PER_ENDPOINT: usize = 2;
/// Their bytes.
pub const ENDPOINT_TRB_BYTES: usize = TRBS_PER_ENDPOINT * TRB_BYTES;

/// The SETUP packet's eight bytes.
pub const SETUP: usize = 2 * PAGE;
/// Endpoint 0's data stage buffer.
pub const EP0_DATA: usize = SETUP + CACHE_LINE;
/// Its size: the largest data stage, a multiple of every control packet
/// size, 64 and 512.
pub const EP0_BYTES: usize = 512;

/// Each other endpoint's buffer.
#[must_use]
pub const fn endpoint_buffer(slot: usize) -> usize {
    (3 + slot) * PAGE
}
/// Its size.
pub const ENDPOINT_BYTES: usize = PAGE;

/// The area.
pub const AREA_BYTES: usize = endpoint_buffer(MAX_ENDPOINTS);

const _: () = assert!(
    TRBS_PER_ENDPOINT * TRB_BYTES <= CACHE_LINE,
    "an endpoint's TRBs fit its line"
);
const _: () = assert!(
    endpoint_trbs(MAX_ENDPOINTS) <= 2 * PAGE,
    "the TRBs fit page 1"
);
const _: () = assert!(EP0_DATA + EP0_BYTES <= 3 * PAGE, "endpoint 0 fits page 2");
