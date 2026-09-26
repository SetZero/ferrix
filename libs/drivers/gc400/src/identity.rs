//! Who the core says it is: the identification registers, read in the order
//! and under the conditions etnaviv's `etnaviv_hw_identify` reads them, and
//! the feature words that decide how the core is driven.
//!
//! # Registers and the database
//!
//! Vivante cores are known to report feature words that are wrong, so
//! etnaviv keeps a database of cores (`etnaviv_hwdb.c`, whose numbers come
//! from Vivante's own feature database) and, when a core's model, revision,
//! product, customer and ECO match an entry, takes that entry's features
//! instead of the registers'. The one entry that matters here is the
//! STM32MP157's GC400T, [`GC400T_STM32MP157`], and [`Identity::features`]
//! makes the same choice for it. The identity line prints what the
//! registers said either way: which of the two the board has is exactly what
//! a first run on it is for.

use core::fmt;

use crate::Registers;
use crate::regs::{features, hi};

/// The chip's identification, as its registers gave it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Identity {
    /// `HI_CHIP_IDENTITY`, which only the oldest cores fill in.
    pub identity: u32,
    /// `HI_CHIP_MODEL`: `0x400` for a GC400. A core of the GC400 family may
    /// report another value in its low byte; see [`Identity::model`].
    pub model: u32,
    /// `HI_CHIP_REV`.
    pub revision: u32,
    /// `HI_CHIP_DATE`.
    pub date: u32,
    /// `HI_CHIP_TIME`.
    pub time: u32,
    /// `HI_CHIP_PRODUCT_ID`.
    pub product: u32,
    /// `HI_CHIP_CUSTOMER_ID`.
    pub customer: u32,
    /// `HI_CHIP_ECO_ID`.
    pub eco: u32,
    /// The feature words the registers gave.
    pub features: Features,
}

/// A core's feature words: the major word and minor words 0 to 5.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Features {
    /// `HI_CHIP_FEATURE`.
    pub major: u32,
    /// `HI_CHIP_MINOR_FEATURE_0` to `_5`; zero where the core has none.
    pub minor: [u32; 6],
}

/// An entry of etnaviv's hardware database: a core and the features it
/// truly has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Known {
    /// The model, as [`Identity::model`] gives it.
    pub model: u32,
    /// The revision.
    pub revision: u32,
    /// The product.
    pub product: u32,
    /// The customer.
    pub customer: u32,
    /// The ECO.
    pub eco: u32,
    /// Its features.
    pub features: Features,
}

/// The STM32MP157's GC400T, as etnaviv's database has it (the first entry
/// of `etnaviv_chip_identities`). It says the core has a 3D pipe and no 2D
/// pipe, the version 1.0 memory controller, and an MMU of version 2.
pub const GC400T_STM32MP157: Known = Known {
    model: 0x400,
    revision: 0x4652,
    product: 0x0007_0001,
    customer: 0x100,
    eco: 0,
    features: Features {
        major: 0xA0E9_E004,
        minor: [
            0xE129_9FFF,
            0xBE13_B219,
            0xCE11_0010,
            0x0800_0001,
            0x0002_0102,
            0x0012_0000,
        ],
    },
};

/// `HI_CHIP_IDENTITY`'s family for a core too old to have the registers
/// after it, which etnaviv takes to be a GC500.
const FAMILY_OLD: u32 = 0x01;

impl Identity {
    /// Read the identification registers.
    ///
    /// A core whose `HI_CHIP_IDENTITY` names the oldest family has no other
    /// identification register, and gives its revision there. A GC600 of
    /// revision `0x19` faults on reading the product and ECO registers, so
    /// they are not read on one. Minor feature word 0 does not exist on a
    /// GC500 before revision 2 or a GC300 before `0x2000`, and words 1 to 5
    /// exist only when word 0 says so.
    pub fn read(registers: &impl Registers) -> Identity {
        let identity = registers.read32(hi::CHIP_IDENTITY);
        let mut found = Identity {
            identity,
            ..Identity::default()
        };
        if identity >> hi::CHIP_IDENTITY_FAMILY_SHIFT == FAMILY_OLD {
            found.model = 0x500;
            found.revision =
                (identity & hi::CHIP_IDENTITY_REVISION_MASK) >> hi::CHIP_IDENTITY_REVISION_SHIFT;
        } else {
            found.model = registers.read32(hi::CHIP_MODEL);
            found.revision = registers.read32(hi::CHIP_REV);
            found.date = registers.read32(hi::CHIP_DATE);
            found.time = registers.read32(hi::CHIP_TIME);
            found.customer = registers.read32(hi::CHIP_CUSTOMER_ID);
            if !(found.model == 0x600 && found.revision == 0x19) {
                found.product = registers.read32(hi::CHIP_PRODUCT_ID);
                found.eco = registers.read32(hi::CHIP_ECO_ID);
            }
        }
        found.features.major = registers.read32(hi::CHIP_FEATURE);
        let no_minor = (found.model == 0x500 && found.revision < 2)
            || (found.model == 0x300 && found.revision < 0x2000);
        if no_minor {
            return found;
        }
        let [first, rest @ ..] = &hi::CHIP_MINOR_FEATURES;
        let [minor0, minor_rest @ ..] = &mut found.features.minor;
        *minor0 = registers.read32(*first);
        if *minor0 & features::MINOR0_MORE_MINOR_FEATURES != 0 {
            for (word, offset) in minor_rest.iter_mut().zip(rest) {
                *word = registers.read32(*offset);
            }
        }
        found
    }

    /// The model as etnaviv compares it: every core of the GC400 family but
    /// the GC420 is a GC400, whatever its low byte says.
    #[must_use]
    pub const fn model(&self) -> u32 {
        if self.model & 0xFF00 == 0x0400 && self.model != 0x420 {
            0x400
        } else {
            self.model
        }
    }

    /// The database entry this core matches, if any.
    #[must_use]
    pub fn known(&self) -> Option<&'static Known> {
        [&GC400T_STM32MP157].into_iter().find(|known| {
            known.model == self.model()
                && known.revision == self.revision
                && known.product == self.product
                && known.customer == self.customer
                && known.eco == self.eco
        })
    }

    /// The features to drive the core by: the database's when it knows the
    /// core, else the registers'.
    #[must_use]
    pub fn features(&self) -> Features {
        self.known().map_or(self.features, |known| known.features)
    }

    /// The bits of `HI_IDLE_STATE` that must all be set for the core to be
    /// idle: every module's, which a GC400 reports as idle when it lacks
    /// the module, but not the bus's low-power bit, which says nothing about
    /// work. (A GC600 or a GC300 reports an absent module as busy, so for
    /// those only the modules they have count.)
    #[must_use]
    pub const fn idle_mask(&self) -> u32 {
        match self.model() {
            0x600 | 0x300 => {
                hi::IDLE_STATE_TX
                    | hi::IDLE_STATE_RA
                    | hi::IDLE_STATE_SE
                    | hi::IDLE_STATE_PA
                    | hi::IDLE_STATE_SH
                    | hi::IDLE_STATE_PE
                    | hi::IDLE_STATE_DE
                    | hi::IDLE_STATE_FE
            }
            _ => !hi::IDLE_STATE_AXI_LP,
        }
    }
}

impl Features {
    /// Whether the MMU is version 2, which starts disabled and passes
    /// physical addresses through until it is turned on.
    #[must_use]
    pub const fn mmu_v2(&self) -> bool {
        self.minor[1] & features::MINOR1_MMU_VERSION != 0
    }

    /// Whether the core has a 3D pipe.
    #[must_use]
    pub const fn pipe_3d(&self) -> bool {
        self.major & features::PIPE_3D != 0
    }

    /// Whether the core has a 2D pipe.
    #[must_use]
    pub const fn pipe_2d(&self) -> bool {
        self.major & features::PIPE_2D != 0
    }

    /// Whether the memory controller is version 2.0.
    #[must_use]
    pub const fn mc20(&self) -> bool {
        self.minor[0] & features::MINOR0_MC20 != 0
    }

    /// Whether the core's clock is scaled by the clock controller rather
    /// than by `HI_CLOCK_CONTROL`'s `FSCALE_VAL`.
    #[must_use]
    pub const fn dynamic_frequency_scaling(&self) -> bool {
        self.minor[2] & features::MINOR2_DYNAMIC_FREQUENCY_SCALING != 0
    }
}

impl fmt::Display for Identity {
    /// Everything the registers said, on one line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "model {:#x} revision {:#x} date {:#010x} time {:#010x} product {:#x} customer {:#x} eco {:#x} identity {:#010x} features {:#010x}",
            self.model,
            self.revision,
            self.date,
            self.time,
            self.product,
            self.customer,
            self.eco,
            self.identity,
            self.features.major
        )?;
        f.write_str(" minor")?;
        for word in self.features.minor {
            write!(f, " {word:#010x}")?;
        }
        Ok(())
    }
}
