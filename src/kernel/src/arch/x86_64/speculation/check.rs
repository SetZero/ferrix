//! The boot's checks of `speculation`'s decisions that need no processor of
//! their kind: every verdict on Zenbleed and Gather Data Sampling, each from
//! a processor described rather than run on.

use super::{
    CAP_GDS_CTRL, CAP_GDS_NO, MCU_GDS_MITG_DIS, MCU_GDS_MITG_LOCKED, VectorFacts, VectorLeak,
    decide_vector_leak, family_and_model,
};

/// A processor for [`vector_leak_decisions`]: `vendor` `b'i'` for
/// Intel, `b'a'` for AMD.
const fn facts(vendor: u8, family: u32, model: u32, hypervisor: bool) -> VectorFacts {
    VectorFacts {
        intel: vendor == b'i',
        amd: vendor == b'a',
        family,
        model,
        hypervisor,
        avx: true,
        capabilities: 0,
        patch_level: 0,
        mcu_opt_ctrl: 0,
    }
}

/// Every verdict [`decide_vector_leak`] can give, each from a processor
/// that must get it, and the family and model read out of two real
/// `CPUID.1:EAX` values -- the decision that keeps AVX from a program where
/// Zenbleed or GDS would leak another's vector registers. With the Zen 2
/// guest's rule taken out of [`super::zenbleed`] (scratch, 2026-09-30) this fails
/// at the guest's row.
///
/// # Errors
///
/// The first processor given a verdict it must not get.
/// Verifies: `L.x86_64.123`
pub(super) fn vector_leak_decisions() -> Result<(), &'static str> {
    let zen2 = facts(b'a', 0x17, 0x31, false);
    let kaby = facts(b'i', 6, 0x8e, false);
    let with = |mut base: VectorFacts, change: fn(&mut VectorFacts)| {
        change(&mut base);
        base
    };
    let cases: [(VectorFacts, VectorLeak); 12] = [
        (zen2, VectorLeak::ZenbleedChickenBit),
        (
            with(zen2, |f| f.patch_level = 0x0830_107b),
            VectorLeak::ZenbleedMicrocode,
        ),
        (
            with(zen2, |f| {
                f.model = 0x90;
                f.patch_level = u32::MAX;
            }),
            VectorLeak::ZenbleedChickenBit,
        ),
        (
            facts(b'a', 0x17, 0x71, true),
            VectorLeak::Uncovered(
                "AVX held back: Zenbleed, on a Zen 2 guest that cannot see the host's fix",
            ),
        ),
        (with(zen2, |f| f.avx = false), VectorLeak::NotAffected),
        (facts(b'a', 0x1a, 0x44, true), VectorLeak::NotAffected),
        (
            kaby,
            VectorLeak::Uncovered("AVX held back: GDS, and no microcode mitigates it"),
        ),
        (
            with(kaby, |f| f.capabilities = CAP_GDS_NO),
            VectorLeak::NotAffected,
        ),
        (
            with(kaby, |f| f.capabilities = CAP_GDS_CTRL),
            VectorLeak::GdsMicrocode,
        ),
        (
            with(kaby, |f| {
                f.capabilities = CAP_GDS_CTRL;
                f.mcu_opt_ctrl = MCU_GDS_MITG_DIS;
            }),
            VectorLeak::GdsMicrocodeEnabled,
        ),
        (
            with(kaby, |f| {
                f.capabilities = CAP_GDS_CTRL;
                f.mcu_opt_ctrl = MCU_GDS_MITG_DIS | MCU_GDS_MITG_LOCKED;
            }),
            VectorLeak::Uncovered("AVX held back: GDS, its microcode mitigation off and locked"),
        ),
        (facts(b'i', 6, 0x97, false), VectorLeak::NotAffected),
    ];
    for (processor, verdict) in cases {
        if decide_vector_leak(&processor) != verdict {
            return Err("a processor was given the wrong verdict on Zenbleed or GDS");
        }
    }
    // A Zen 2 Matisse and a Kaby Lake, as CPUID reports them.
    if family_and_model(0x0087_0f10) != (0x17, 0x71) || family_and_model(0x0008_06ea) != (6, 0x8e) {
        return Err("a processor's family and model were read wrongly for Zenbleed or GDS");
    }
    Ok(())
}
