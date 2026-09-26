//! The one source of unpredictability the net core needs.
//!
//! Two things must not be guessable by a stranger: a connection's initial
//! sequence number, and the source port a query goes out from. Both are
//! defences against an off-path attacker who can send packets but not see
//! them -- RFC 6528 for the first, RFC 5452 for the second -- and both are
//! worthless if the numbers come from a counter.
//!
//! This is not a cryptographic generator and does not pretend to be. It is
//! `SplitMix64`, seeded by the kernel from whatever real entropy the machine
//! has, which turns "the next port is the last plus one" into "the next port
//! is unrelated to the last". A stack seeded with zero -- a test, or a boot
//! before an entropy source exists -- is deterministic, and says so here
//! rather than looking random and not being.

/// A small, fast, seedable generator.
#[derive(Clone, Copy, Debug, Default)]
pub struct Random {
    /// The state, advanced by the golden-ratio constant on every draw.
    state: u64,
}

impl Random {
    /// A generator that will produce the same sequence for the same seed.
    #[must_use]
    pub const fn new(seed: u64) -> Random {
        Random { state: seed }
    }

    /// Start again from a new seed.
    pub const fn reseed(&mut self, seed: u64) {
        self.state = seed;
    }

    /// The next 64 bits: `SplitMix64`, from Steele, Lea and Flood.
    pub const fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// The next 32 bits.
    pub const fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// A number in `low..=high`, or `low` if the range is empty.
    pub const fn in_range(&mut self, low: u16, high: u16) -> u16 {
        if high <= low {
            return low;
        }
        let span = (high - low) as u32 + 1;
        low + (self.next_u32() % span) as u16
    }
}
