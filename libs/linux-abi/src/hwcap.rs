//! `AT_HWCAP` and `AT_HWCAP2`: what a program is told the processor can do.
//!
//! A libc reads these before `main` and chooses code paths by them, so a zero
//! is not a harmless omission. musl's ARMv7-A `setjmp` and `longjmp` save and
//! restore `d8`-`d15` only when [`arm::HWCAP_VFP`] is set: a hard-float program
//! told nothing about its FPU loses its callee-saved double registers across
//! every `longjmp`, and busybox's shell recovers from every error with one.
//!
//! # Where the numbers come from
//!
//! The bit values are Linux's `arch/arm/include/uapi/asm/hwcap.h` and
//! `arch/arm64/include/uapi/asm/hwcap.h`. The copy they were checked against is
//! QEMU's `linux-user/elfload.c`, whose `ARM_HWCAP_ARM_*` and `ARM_HWCAP_A64_*`
//! enums transcribe those headers; QEMU's `linux-headers/` does not carry
//! `hwcap.h`. Which identification register field grants a bit, and at what
//! value, is from QEMU's `target/arm/cpu.h` (the `FIELD` positions) and
//! `target/arm/cpu-features.h` (the tests of them).
//!
//! # What is left out, on purpose
//!
//! A bit is a promise the *kernel* keeps as well as the core, so only features
//! a program can use without the kernel doing anything are reported. No SVE or
//! SME (their state is neither enabled nor saved), no pointer authentication
//! (no keys are installed), no BTI or MTE (no page attributes for them), no
//! `CPUID` (reading ID registers from EL0 is not emulated), no `EVTSTRM` (the
//! timer's event stream is off), and on ARMv7-A no `SWP` (not emulated) and no
//! `ThumbEE` (its register is not switched).

/// ARMv7-A (EABI) bits and how to derive them.
pub mod arm {
    /// Half-word loads and stores.
    pub const HWCAP_HALF: u32 = 1 << 1;
    /// Thumb instructions.
    pub const HWCAP_THUMB: u32 = 1 << 2;
    /// Long multiplies.
    pub const HWCAP_FAST_MULT: u32 = 1 << 4;
    /// A VFP floating-point unit.
    pub const HWCAP_VFP: u32 = 1 << 6;
    /// The DSP extension.
    pub const HWCAP_EDSP: u32 = 1 << 7;
    /// Advanced SIMD.
    pub const HWCAP_NEON: u32 = 1 << 12;
    /// `VFPv3`.
    pub const HWCAP_VFPV3: u32 = 1 << 13;
    /// `VFPv3` with sixteen double registers only.
    pub const HWCAP_VFPV3D16: u32 = 1 << 14;
    /// A user-readable thread register, `TPIDRURO`.
    pub const HWCAP_TLS: u32 = 1 << 15;
    /// `VFPv4`: fused multiply-accumulate.
    pub const HWCAP_VFPV4: u32 = 1 << 16;
    /// Hardware divide in ARM state.
    pub const HWCAP_IDIVA: u32 = 1 << 17;
    /// Hardware divide in Thumb state.
    pub const HWCAP_IDIVT: u32 = 1 << 18;
    /// Thirty-two double registers.
    pub const HWCAP_VFPD32: u32 = 1 << 19;
    /// The Large Physical Address Extension.
    pub const HWCAP_LPAE: u32 = 1 << 20;

    /// The identification registers the bits are derived from.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct IdRegisters {
        /// `ID_ISAR0`.
        pub id_isar0: u32,
        /// `ID_MMFR0`.
        pub id_mmfr0: u32,
        /// `MVFR0`, or zero when there is no FPU to read it from.
        pub mvfr0: u32,
        /// `MVFR1`, or zero when there is no FPU to read it from.
        pub mvfr1: u32,
    }

    /// The four-bit field at `shift`.
    const fn field(register: u32, shift: u32) -> u32 {
        (register >> shift) & 0xF
    }

    /// `AT_HWCAP` for a core with these registers.
    ///
    /// Half-word transfers, Thumb, long multiplies, the DSP extension and
    /// `TPIDRURO` are part of ARMv7-A itself, so every core this kernel runs
    /// on has them.
    #[must_use]
    pub const fn hwcap(ids: IdRegisters) -> u32 {
        let mut bits = HWCAP_HALF | HWCAP_THUMB | HWCAP_FAST_MULT | HWCAP_EDSP | HWCAP_TLS;

        // ID_ISAR0.Divide, bits 27:24: 1 is Thumb only, 2 adds ARM.
        let divide = field(ids.id_isar0, 24);
        if divide != 0 {
            bits |= HWCAP_IDIVT;
        }
        if divide > 1 {
            bits |= HWCAP_IDIVA;
        }
        // ID_MMFR0.VMSA, bits 3:0: 5 is VMSAv7 with LPAE.
        if field(ids.id_mmfr0, 0) >= 5 {
            bits |= HWCAP_LPAE;
        }

        // MVFR0: SIMDReg 3:0, FPSP 7:4, FPDP 11:8.
        let registers = field(ids.mvfr0, 0);
        let single = field(ids.mvfr0, 4);
        let double = field(ids.mvfr0, 8);
        if single > 0 || double > 0 {
            bits |= HWCAP_VFP;
        }
        if single >= 2 || double >= 2 {
            bits |= HWCAP_VFPV3;
            bits |= if registers >= 2 {
                HWCAP_VFPD32
            } else {
                HWCAP_VFPV3D16
            };
        }
        // MVFR1: SIMDLS 11:8, SIMDInt 15:12, SIMDSP 19:16, SIMDFMAC 31:28.
        if field(ids.mvfr1, 8) != 0 && field(ids.mvfr1, 12) != 0 && field(ids.mvfr1, 16) != 0 {
            bits |= HWCAP_NEON;
        }
        if field(ids.mvfr1, 28) != 0 {
            bits |= HWCAP_VFPV4;
        }
        bits
    }
}

/// AArch64 bits and how to derive them.
pub mod aarch64 {
    /// Floating point.
    pub const HWCAP_FP: u64 = 1 << 0;
    /// Advanced SIMD.
    pub const HWCAP_ASIMD: u64 = 1 << 1;
    /// AES instructions.
    pub const HWCAP_AES: u64 = 1 << 3;
    /// Polynomial multiply.
    pub const HWCAP_PMULL: u64 = 1 << 4;
    /// SHA-1 instructions.
    pub const HWCAP_SHA1: u64 = 1 << 5;
    /// SHA-256 instructions.
    pub const HWCAP_SHA2: u64 = 1 << 6;
    /// CRC-32 instructions.
    pub const HWCAP_CRC32: u64 = 1 << 7;
    /// Large System Extensions: atomic compare-and-swap instructions.
    pub const HWCAP_ATOMICS: u64 = 1 << 8;
    /// Half-precision floating point.
    pub const HWCAP_FPHP: u64 = 1 << 9;
    /// Half-precision Advanced SIMD.
    pub const HWCAP_ASIMDHP: u64 = 1 << 10;
    /// Rounding double multiply accumulate.
    pub const HWCAP_ASIMDRDM: u64 = 1 << 12;
    /// JavaScript conversion.
    pub const HWCAP_JSCVT: u64 = 1 << 13;
    /// Complex multiply accumulate.
    pub const HWCAP_FCMA: u64 = 1 << 14;
    /// `LDAPR`: loads with release-consistent processor-correct semantics.
    pub const HWCAP_LRCPC: u64 = 1 << 15;
    /// `DC CVAP`.
    pub const HWCAP_DCPOP: u64 = 1 << 16;
    /// SHA-3 instructions.
    pub const HWCAP_SHA3: u64 = 1 << 17;
    /// SM3 instructions.
    pub const HWCAP_SM3: u64 = 1 << 18;
    /// SM4 instructions.
    pub const HWCAP_SM4: u64 = 1 << 19;
    /// Dot product.
    pub const HWCAP_ASIMDDP: u64 = 1 << 20;
    /// SHA-512 instructions.
    pub const HWCAP_SHA512: u64 = 1 << 21;
    /// FP16 multiply-accumulate.
    pub const HWCAP_ASIMDFHM: u64 = 1 << 23;
    /// The rest of the `LRCPC` instructions.
    pub const HWCAP_ILRCPC: u64 = 1 << 26;
    /// Flag manipulation instructions.
    pub const HWCAP_FLAGM: u64 = 1 << 27;
    /// Speculation barrier.
    pub const HWCAP_SB: u64 = 1 << 29;

    /// `DC CVADP`.
    pub const HWCAP2_DCPODP: u64 = 1 << 0;
    /// The second flag manipulation extension.
    pub const HWCAP2_FLAGM2: u64 = 1 << 7;
    /// Rounding to 32- and 64-bit integers.
    pub const HWCAP2_FRINT: u64 = 1 << 8;
    /// 8-bit integer matrix multiply.
    pub const HWCAP2_I8MM: u64 = 1 << 13;
    /// `BFloat16`.
    pub const HWCAP2_BF16: u64 = 1 << 14;
    /// `RNDR` and `RNDRRS`.
    pub const HWCAP2_RNG: u64 = 1 << 16;

    /// The identification registers the bits are derived from.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct IdRegisters {
        /// `ID_AA64PFR0_EL1`.
        pub pfr0: u64,
        /// `ID_AA64ISAR0_EL1`.
        pub isar0: u64,
        /// `ID_AA64ISAR1_EL1`.
        pub isar1: u64,
    }

    /// The four-bit field at `shift`.
    const fn field(register: u64, shift: u32) -> u64 {
        (register >> shift) & 0xF
    }

    /// Set `bit` when `value` is at least `least`.
    const fn when(value: u64, least: u64, bit: u64) -> u64 {
        if value >= least { bit } else { 0 }
    }

    /// `AT_HWCAP` for a core with these registers.
    #[must_use]
    pub const fn hwcap(ids: IdRegisters) -> u64 {
        // ID_AA64PFR0 FP 19:16 and AdvSIMD 23:20 are signed: 0xF is absent, 0
        // present, 1 present with half precision.
        let fp = field(ids.pfr0, 16);
        let simd = field(ids.pfr0, 20);
        let mut bits = 0;
        if fp != 0xF {
            bits |= HWCAP_FP | when(fp, 1, HWCAP_FPHP);
        }
        if simd != 0xF {
            bits |= HWCAP_ASIMD | when(simd, 1, HWCAP_ASIMDHP);
        }

        let isar0 = ids.isar0;
        bits |= when(field(isar0, 4), 1, HWCAP_AES) | when(field(isar0, 4), 2, HWCAP_PMULL);
        bits |= when(field(isar0, 8), 1, HWCAP_SHA1);
        bits |= when(field(isar0, 12), 1, HWCAP_SHA2) | when(field(isar0, 12), 2, HWCAP_SHA512);
        bits |= when(field(isar0, 16), 1, HWCAP_CRC32);
        bits |= when(field(isar0, 20), 1, HWCAP_ATOMICS);
        bits |= when(field(isar0, 28), 1, HWCAP_ASIMDRDM);
        bits |= when(field(isar0, 32), 1, HWCAP_SHA3);
        bits |= when(field(isar0, 36), 1, HWCAP_SM3);
        bits |= when(field(isar0, 40), 1, HWCAP_SM4);
        bits |= when(field(isar0, 44), 1, HWCAP_ASIMDDP);
        bits |= when(field(isar0, 48), 1, HWCAP_ASIMDFHM);
        bits |= when(field(isar0, 52), 1, HWCAP_FLAGM);

        let isar1 = ids.isar1;
        bits |= when(field(isar1, 0), 1, HWCAP_DCPOP);
        bits |= when(field(isar1, 12), 1, HWCAP_JSCVT);
        bits |= when(field(isar1, 16), 1, HWCAP_FCMA);
        bits |= when(field(isar1, 20), 1, HWCAP_LRCPC) | when(field(isar1, 20), 2, HWCAP_ILRCPC);
        bits |= when(field(isar1, 36), 1, HWCAP_SB);
        bits
    }

    /// `AT_HWCAP2` for a core with these registers.
    #[must_use]
    pub const fn hwcap2(ids: IdRegisters) -> u64 {
        when(field(ids.isar1, 0), 2, HWCAP2_DCPODP)
            | when(field(ids.isar0, 52), 2, HWCAP2_FLAGM2)
            | when(field(ids.isar1, 32), 1, HWCAP2_FRINT)
            | when(field(ids.isar1, 52), 1, HWCAP2_I8MM)
            | when(field(ids.isar1, 44), 1, HWCAP2_BF16)
            | when(field(ids.isar0, 60), 1, HWCAP2_RNG)
    }
}
