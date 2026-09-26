//! `HW_REFINE` and `HW_PARAMS`: narrowing a configuration space to what a
//! card offers.
//!
//! alsa-lib's `hw` plugin solves no constraints itself: every `set_*` a
//! program calls is a `HW_REFINE` round trip, and `set_*_near` tries a
//! minimum, then a maximum, and restores the space on `EINVAL`
//! (`docs/AUDIO.md` §2.1). So what the kernel answers has to be Linux's
//! answer to the bit, and this module is Linux's rules, read from
//! `sound/core/pcm_lib.c` (`snd_interval_refine`, `snd_interval_refine_first`)
//! and `sound/core/pcm_native.c` (`snd_pcm_hw_refine`,
//! `constrain_mask_params`, `constrain_interval_params`,
//! `fixup_unreferenced_params`, `snd_pcm_hw_params_choose`), copied to
//! `~/.local/share/ferrix/audio-ref/kernel/` on 2026-09-26.
//!
//! # One departure, and why it changes no answer
//!
//! Linux refines only the parameters whose bit the caller set in `rmask`, and
//! its rules (`FRAME_BITS` is `SAMPLE_BITS` times `CHANNELS`, and the rest)
//! then carry a change on to the parameters that depend on it. A card with one
//! configuration has every parameter fixed by any other, so the rules would
//! narrow every parameter to its value whichever one was asked; this module
//! narrows all of them on every refine instead of running rules. An empty
//! parameter is refused wherever it is, as `constrain_*_params` refuses it
//! before looking at `rmask`, and `cmask` gains the bit of every parameter
//! that changed, as a rule's target gains it.

use ferrix_linux_abi::sound::{
    HW_PARAM_FIRST_INTERVAL, HW_PARAM_FIRST_MASK, HW_PARAM_FORMAT, HW_PARAM_RATE,
    HW_PARAM_SAMPLE_BITS, HwParams, INTERVAL_EMPTY, INTERVAL_INTEGER, INTERVAL_OPENMAX,
    INTERVAL_OPENMIN, INTERVALS, Interval, MASKS, Mask,
};

/// A refine that leaves some parameter with nothing in it: `EINVAL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Empty;

// ---------------------------------------------------------------------------
// Intervals
// ---------------------------------------------------------------------------

/// `snd_interval_any`: every value.
pub const ANY: Interval = Interval {
    min: 0,
    max: u32::MAX,
    flags: 0,
};

/// The interval holding `value` alone, as a whole number.
#[must_use]
pub const fn single(value: u32) -> Interval {
    Interval {
        min: value,
        max: value,
        flags: INTERVAL_INTEGER,
    }
}

const fn has(interval: &Interval, flag: u32) -> bool {
    interval.flags & flag != 0
}

/// `snd_interval_empty`: the `empty` flag, which is all Linux looks at.
#[must_use]
pub const fn is_empty(interval: &Interval) -> bool {
    has(interval, INTERVAL_EMPTY)
}

/// `snd_interval_checkempty`: whether the bounds hold nothing.
const fn holds_nothing(interval: &Interval) -> bool {
    interval.min > interval.max
        || (interval.min == interval.max
            && (has(interval, INTERVAL_OPENMIN) || has(interval, INTERVAL_OPENMAX)))
}

/// `snd_interval_single`: whether exactly one value is in it.
#[must_use]
pub const fn is_single(interval: &Interval) -> bool {
    !is_empty(interval)
        && (interval.min == interval.max
            || (interval.min.wrapping_add(1) == interval.max
                && (has(interval, INTERVAL_OPENMIN) || has(interval, INTERVAL_OPENMAX))))
}

/// `snd_interval_value`: the value of a single interval.
#[must_use]
pub const fn value(interval: &Interval) -> u32 {
    if has(interval, INTERVAL_OPENMIN) && !has(interval, INTERVAL_OPENMAX) {
        interval.max
    } else {
        interval.min
    }
}

fn set(interval: &mut Interval, flag: u32, on: bool) {
    if on {
        interval.flags |= flag;
    } else {
        interval.flags &= !flag;
    }
}

/// `snd_interval_refine`: narrow `interval` to what `by` allows, and say
/// whether it changed.
///
/// # Errors
///
/// [`Empty`] if `interval` was empty to start with or nothing is left, in
/// which case it is marked empty, as `snd_interval_none` marks it.
pub fn refine_interval(interval: &mut Interval, by: &Interval) -> Result<bool, Empty> {
    if is_empty(interval) {
        return Err(Empty);
    }
    let mut changed = false;
    if interval.min < by.min {
        interval.min = by.min;
        set(interval, INTERVAL_OPENMIN, has(by, INTERVAL_OPENMIN));
        changed = true;
    } else if interval.min == by.min
        && !has(interval, INTERVAL_OPENMIN)
        && has(by, INTERVAL_OPENMIN)
    {
        set(interval, INTERVAL_OPENMIN, true);
        changed = true;
    }
    if interval.max > by.max {
        interval.max = by.max;
        set(interval, INTERVAL_OPENMAX, has(by, INTERVAL_OPENMAX));
        changed = true;
    } else if interval.max == by.max
        && !has(interval, INTERVAL_OPENMAX)
        && has(by, INTERVAL_OPENMAX)
    {
        set(interval, INTERVAL_OPENMAX, true);
        changed = true;
    }
    if !has(interval, INTERVAL_INTEGER) && has(by, INTERVAL_INTEGER) {
        set(interval, INTERVAL_INTEGER, true);
        changed = true;
    }
    if has(interval, INTERVAL_INTEGER) {
        // An open end of a whole-number interval is the next whole number in.
        // C's unsigned arithmetic wraps here, and a wrapped bound is caught
        // as empty just below, as it is in C.
        if has(interval, INTERVAL_OPENMIN) {
            interval.min = interval.min.wrapping_add(1);
            set(interval, INTERVAL_OPENMIN, false);
        }
        if has(interval, INTERVAL_OPENMAX) {
            interval.max = interval.max.wrapping_sub(1);
            set(interval, INTERVAL_OPENMAX, false);
        }
    } else if !has(interval, INTERVAL_OPENMIN)
        && !has(interval, INTERVAL_OPENMAX)
        && interval.min == interval.max
    {
        set(interval, INTERVAL_INTEGER, true);
    }
    if holds_nothing(interval) {
        set(interval, INTERVAL_EMPTY, true);
        return Err(Empty);
    }
    Ok(changed)
}

/// `snd_interval_refine_first`: keep only the smallest value.
///
/// # Errors
///
/// [`Empty`] for an empty interval.
pub fn refine_first(interval: &mut Interval) -> Result<bool, Empty> {
    if is_empty(interval) {
        return Err(Empty);
    }
    if is_single(interval) {
        return Ok(false);
    }
    let last_max = interval.max;
    interval.max = interval.min;
    if has(interval, INTERVAL_OPENMIN) {
        interval.max = interval.max.wrapping_add(1);
    }
    // Only exclude the maximum if it was excluded before, as Linux does.
    let open = has(interval, INTERVAL_OPENMAX) && interval.max >= last_max;
    set(interval, INTERVAL_OPENMAX, open);
    Ok(true)
}

// ---------------------------------------------------------------------------
// Masks
// ---------------------------------------------------------------------------

/// The mask holding `bit` alone.
#[must_use]
pub const fn only(bit: u32) -> Mask {
    const fn word(bit: u32, index: u32) -> u32 {
        if bit / 32 == index {
            1 << (bit % 32)
        } else {
            0
        }
    }
    Mask {
        bits: [
            word(bit, 0),
            word(bit, 1),
            word(bit, 2),
            word(bit, 3),
            word(bit, 4),
            word(bit, 5),
            word(bit, 6),
            word(bit, 7),
        ],
    }
}

/// `snd_mask_empty`.
#[must_use]
pub fn mask_empty(mask: &Mask) -> bool {
    mask.bits.iter().all(|word| *word == 0)
}

/// `snd_mask_single`: exactly one bit set.
#[must_use]
pub fn mask_single(mask: &Mask) -> bool {
    mask.bits.iter().map(|word| word.count_ones()).sum::<u32>() == 1
}

/// `snd_mask_min`: the lowest bit set, if any.
#[must_use]
pub fn mask_min(mask: &Mask) -> Option<u32> {
    mask.bits
        .iter()
        .zip(0_u32..)
        .find(|(word, _)| **word != 0)
        .map(|(word, index)| index * 32 + word.trailing_zeros())
}

/// `snd_mask_refine`: intersect `mask` with `by`, and say whether it changed.
///
/// # Errors
///
/// [`Empty`] if nothing is left.
pub fn refine_mask(mask: &mut Mask, by: &Mask) -> Result<bool, Empty> {
    let old = *mask;
    for (word, allowed) in mask.bits.iter_mut().zip(by.bits) {
        *word &= allowed;
    }
    if mask_empty(mask) {
        return Err(Empty);
    }
    Ok(*mask != old)
}

/// `snd_mask_refine_first`: keep only the lowest bit.
fn mask_first(mask: &mut Mask) -> bool {
    match mask_min(mask) {
        Some(bit) if !mask_single(mask) => {
            *mask = only(bit);
            true
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// The whole space
// ---------------------------------------------------------------------------

/// What a card offers: one mask per mask parameter and one interval per
/// interval parameter, as `substream->runtime->hw_constraints` holds them,
/// with what it reports in `info`, `msbits` and `fifo_size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Constraints {
    /// Access, format, subformat.
    pub masks: [Mask; MASKS],
    /// `SAMPLE_BITS` to `TICK_TIME`.
    pub intervals: [Interval; INTERVALS],
    /// `INFO_*` the card reports, without mapped access.
    pub info: u32,
}

/// The bit of parameter `param` in `rmask` and `cmask`.
#[must_use]
pub const fn param_bit(param: u32) -> u32 {
    1 << param
}

/// The bits of every parameter, as `HW_PARAMS` sets `rmask`.
pub const ALL_PARAMS: u32 = u32::MAX;

/// `snd_pcm_format_width` for the formats a card here offers: the bits of a
/// sample that carry sound.
fn format_width(format: u32) -> Option<u32> {
    use ferrix_linux_abi::sound::{
        FORMAT_FLOAT_LE, FORMAT_S8, FORMAT_S16_LE, FORMAT_S32_LE, FORMAT_U8, FORMAT_U16_LE,
        FORMAT_U32_LE,
    };
    match format {
        FORMAT_S8 | FORMAT_U8 => Some(8),
        FORMAT_S16_LE | FORMAT_U16_LE => Some(16),
        FORMAT_S32_LE | FORMAT_U32_LE | FORMAT_FLOAT_LE => Some(32),
        _ => None,
    }
}

/// `snd_pcm_hw_refine` followed by `fixup_unreferenced_params`: what
/// `HW_REFINE` does to `params`. On an error `params` holds a half-refined
/// space, which the caller does not copy back, as Linux does not.
///
/// # Errors
///
/// [`Empty`] when some parameter has nothing left: `EINVAL`.
pub fn refine(params: &mut HwParams, by: &Constraints) -> Result<(), Empty> {
    params.info = 0;
    params.fifo_size = 0;
    if params.rmask & param_bit(HW_PARAM_SAMPLE_BITS) != 0 {
        params.msbits = 0;
    }
    if params.rmask & param_bit(HW_PARAM_RATE) != 0 {
        params.rate_num = 0;
        params.rate_den = 0;
    }
    for ((mask, allowed), param) in params
        .masks
        .iter_mut()
        .zip(&by.masks)
        .zip(HW_PARAM_FIRST_MASK..)
    {
        if mask_empty(mask) {
            return Err(Empty);
        }
        if refine_mask(mask, allowed)? {
            params.cmask |= param_bit(param);
        }
    }
    for ((interval, allowed), param) in params
        .intervals
        .iter_mut()
        .zip(&by.intervals)
        .zip(HW_PARAM_FIRST_INTERVAL..)
    {
        if refine_interval(interval, allowed)? {
            params.cmask |= param_bit(param);
        }
    }
    params.rmask = 0;
    fixup(params, by);
    Ok(())
}

/// `fixup_unreferenced_params`, for a card whose FIFO is not counted and
/// whose sync id is not set.
fn fixup(params: &mut HwParams, by: &Constraints) {
    let format = params
        .masks
        .get((HW_PARAM_FORMAT - HW_PARAM_FIRST_MASK) as usize)
        .copied();
    let single_format = format.filter(mask_single).and_then(|mask| mask_min(&mask));
    if params.msbits == 0 {
        let bits = params
            .intervals
            .get((HW_PARAM_SAMPLE_BITS - HW_PARAM_FIRST_INTERVAL) as usize)
            .filter(|interval| is_single(interval))
            .map(value);
        if let Some(bits) = bits {
            params.msbits = bits;
        }
        if let Some(width) = single_format.and_then(format_width) {
            params.msbits = width;
        }
    }
    if params.rate_den == 0 {
        let rate = params
            .intervals
            .get((HW_PARAM_RATE - HW_PARAM_FIRST_INTERVAL) as usize)
            .filter(|interval| is_single(interval))
            .map(value);
        if let Some(rate) = rate {
            params.rate_num = rate;
            params.rate_den = 1;
        }
    }
    // `fifo_size` stays 0: the card reports no FIFO. `info` was cleared at
    // the start, so it is always the card's.
    params.info = by.info;
}

/// `snd_pcm_hw_params`' refine and choice: every parameter refined, then each
/// one still holding more than one value narrowed to its first, then the
/// fixups. What `HW_PARAMS` returns, before the stream takes the settings.
///
/// # Errors
///
/// [`Empty`] when some parameter has nothing left: `EINVAL`.
pub fn choose(params: &mut HwParams, by: &Constraints) -> Result<(), Empty> {
    params.rmask = ALL_PARAMS;
    refine(params, by)?;
    for mask in &mut params.masks {
        let _ = mask_first(mask);
    }
    for interval in &mut params.intervals {
        let _ = refine_first(interval)?;
    }
    fixup(params, by);
    Ok(())
}
