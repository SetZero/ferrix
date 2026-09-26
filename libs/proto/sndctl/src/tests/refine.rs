//! The refine: Linux's interval and mask rules, and what alsa-lib's calls
//! come back with against version 1's one configuration.
//!
//! alsa-lib's `hw_params_any` sends every mask full, every interval
//! `[0, UINT_MAX]`, `rmask` and `info` all ones (`docs/AUDIO.md` §2.1); a
//! `set_*` sends one parameter narrowed with its bit in `rmask`; a
//! `set_*_near` tries the value as a minimum and as a maximum on copies and
//! keeps whichever the kernel does not refuse.

use ferrix_linux_abi::sound::{
    ACCESS_MMAP_INTERLEAVED, ACCESS_RW_INTERLEAVED, FORMAT_FLOAT_LE, FORMAT_S16_LE,
    HW_PARAM_ACCESS, HW_PARAM_BUFFER_TIME, HW_PARAM_CHANNELS, HW_PARAM_FIRST_INTERVAL,
    HW_PARAM_FORMAT, HW_PARAM_PERIOD_TIME, HW_PARAM_RATE, HW_PARAM_TICK_TIME, HwParams,
    INTERVAL_EMPTY, INTERVAL_INTEGER, INTERVAL_OPENMAX, INTERVAL_OPENMIN, Interval, Mask,
};

use crate::pcm::{INFO, VERSION_1};
use crate::refine::{
    ANY, Empty, choose, is_single, only, param_bit, refine, refine_first, refine_interval,
    refine_mask, single, value,
};

/// What `snd_pcm_hw_params_any` sends.
fn any() -> HwParams {
    HwParams {
        flags: 0,
        masks: [Mask {
            bits: [u32::MAX; 8],
        }; 3],
        mres: [Mask { bits: [0; 8] }; 5],
        intervals: [ANY; 12],
        ires: [Interval {
            min: 0,
            max: 0,
            flags: 0,
        }; 9],
        rmask: u32::MAX,
        cmask: 0,
        info: u32::MAX,
        msbits: 0,
        rate_num: 0,
        rate_den: 0,
        fifo_size: 0,
        sync: [0; 16],
        reserved: [0; 48],
    }
}

fn interval(params: &HwParams, param: u32) -> Interval {
    params.intervals[(param - HW_PARAM_FIRST_INTERVAL) as usize]
}

fn narrowed(mut params: HwParams, param: u32, to: Interval) -> HwParams {
    params.intervals[(param - HW_PARAM_FIRST_INTERVAL) as usize] = to;
    params.rmask = param_bit(param);
    params
}

fn range(min: u32, max: u32, flags: u32) -> Interval {
    Interval { min, max, flags }
}

#[test]
fn version_1_is_the_configuration_the_document_names() {
    let config = VERSION_1;
    assert_eq!(config.frame_bytes(), 4);
    assert_eq!(config.buffer_frames(), 3840);
    assert_eq!(config.period_bytes(), 3840);
    assert_eq!(config.buffer_bytes(), 15360);
    assert_eq!(config.period_time(), 20_000);
    assert_eq!(config.buffer_time(), 80_000);
    assert!(config.times_are_whole(), "every interval is closed");
}

#[test]
fn any_space_comes_back_as_the_one_configuration() {
    let mut params = any();
    refine(&mut params, &VERSION_1.constraints()).expect("the whole space holds it");
    assert_eq!(params.masks[0], only(ACCESS_RW_INTERLEAVED));
    assert_eq!(params.masks[1], only(FORMAT_S16_LE));
    let expected = [16, 32, 2, 48_000, 20_000, 960, 3840, 4, 80_000, 3840, 15360];
    for (index, want) in expected.into_iter().enumerate() {
        let got = params.intervals[index];
        assert!(is_single(&got), "parameter {} is single", index + 8);
        assert_eq!(value(&got), want, "parameter {}", index + 8);
    }
    // TICK_TIME is not constrained: it comes back as it went.
    assert_eq!(interval(&params, HW_PARAM_TICK_TIME), ANY);
    assert_eq!(params.rmask, 0);
    // Every parameter changed but TICK_TIME, which is not constrained.
    assert_ne!(params.cmask & param_bit(HW_PARAM_ACCESS), 0);
    assert_ne!(params.cmask & param_bit(HW_PARAM_RATE), 0);
    assert_eq!(params.cmask & param_bit(HW_PARAM_TICK_TIME), 0);
    assert_eq!(params.info, INFO, "info is the card's, not the ones sent");
    assert_eq!(params.msbits, 16);
    assert_eq!((params.rate_num, params.rate_den), (48_000, 1));
    assert_eq!(params.fifo_size, 0);
}

#[test]
fn a_set_that_agrees_changes_nothing_and_one_that_does_not_is_einval() {
    let constraints = VERSION_1.constraints();
    let mut base = any();
    refine(&mut base, &constraints).expect("any");

    // `set_access(RW_INTERLEAVED)`, then PipeWire's first try, mapped access.
    for (access, ok) in [
        (ACCESS_RW_INTERLEAVED, true),
        (ACCESS_MMAP_INTERLEAVED, false),
    ] {
        let mut params = base;
        params.masks[0] = only(access);
        params.rmask = param_bit(HW_PARAM_ACCESS);
        params.cmask = 0;
        assert_eq!(
            refine(&mut params, &constraints).is_ok(),
            ok,
            "access {access}"
        );
        if ok {
            assert_eq!(params.cmask, 0, "nothing changed");
        }
    }
    let mut params = base;
    params.masks[1] = only(FORMAT_FLOAT_LE);
    params.rmask = param_bit(HW_PARAM_FORMAT);
    assert_eq!(refine(&mut params, &constraints), Err(Empty));

    let mut params = narrowed(base, HW_PARAM_CHANNELS, single(1));
    assert_eq!(refine(&mut params, &constraints), Err(Empty), "mono");
}

#[test]
fn set_near_finds_the_rate_and_the_times_from_either_side() {
    let constraints = VERSION_1.constraints();
    let mut base = any();
    refine(&mut base, &constraints).expect("any");
    let try_refine = |params: HwParams| {
        let mut params = params;
        refine(&mut params, &constraints).map(|()| params)
    };

    // `set_rate_near(44100)`: as a minimum it holds 48000, as a maximum
    // nothing.
    let up = try_refine(narrowed(base, HW_PARAM_RATE, range(44_100, u32::MAX, 0)))
        .expect("48000 is above 44100");
    assert_eq!(value(&interval(&up, HW_PARAM_RATE)), 48_000);
    assert_eq!(
        try_refine(narrowed(base, HW_PARAM_RATE, range(0, 44_100, 0))),
        Err(Empty)
    );

    // aplay's `buffer_time_near(500000)`: only from below.
    let down = try_refine(narrowed(base, HW_PARAM_BUFFER_TIME, range(0, 500_000, 0)))
        .expect("80000 is below 500000");
    assert_eq!(value(&interval(&down, HW_PARAM_BUFFER_TIME)), 80_000);
    assert_eq!(
        try_refine(narrowed(
            base,
            HW_PARAM_BUFFER_TIME,
            range(500_000, u32::MAX, 0)
        )),
        Err(Empty)
    );

    // An open interval around the period time holds it; one open at it does
    // not.
    let around = narrowed(
        base,
        HW_PARAM_PERIOD_TIME,
        range(19_999, 20_001, INTERVAL_OPENMIN | INTERVAL_OPENMAX),
    );
    assert!(try_refine(around).is_ok());
    let past = narrowed(
        base,
        HW_PARAM_PERIOD_TIME,
        range(20_000, 20_001, INTERVAL_OPENMIN),
    );
    assert_eq!(try_refine(past), Err(Empty), "20000 itself is left out");
}

#[test]
fn an_empty_parameter_is_refused_wherever_it_is() {
    let constraints = VERSION_1.constraints();
    let mut params = any();
    params.intervals[11] = range(5, 5, INTERVAL_EMPTY);
    params.rmask = 0;
    assert_eq!(refine(&mut params, &constraints), Err(Empty));
    let mut params = any();
    params.masks[2] = Mask { bits: [0; 8] };
    params.rmask = 0;
    assert_eq!(refine(&mut params, &constraints), Err(Empty));
}

#[test]
fn intervals_refine_as_snd_interval_refine_does() {
    // Closed into a single whole number.
    let mut one = range(10, 20, 0);
    assert_eq!(refine_interval(&mut one, &single(15)), Ok(true));
    assert_eq!(one, range(15, 15, INTERVAL_INTEGER));
    assert_eq!(
        refine_interval(&mut one, &single(15)),
        Ok(false),
        "no change"
    );

    // Open ends of a whole-number interval move inward.
    let mut open = range(10, 20, INTERVAL_OPENMIN | INTERVAL_OPENMAX);
    assert_eq!(
        refine_interval(&mut open, &range(0, 100, INTERVAL_INTEGER)),
        Ok(true)
    );
    assert_eq!(open, range(11, 19, INTERVAL_INTEGER));

    // A single value that is not marked whole becomes whole.
    let mut point = range(7, 7, 0);
    assert_eq!(refine_interval(&mut point, &range(0, 100, 0)), Ok(false));
    assert_eq!(point.flags, INTERVAL_INTEGER);

    // Disjoint: empty, and marked so.
    let mut apart = range(1, 2, 0);
    assert_eq!(refine_interval(&mut apart, &range(3, 4, 0)), Err(Empty));
    assert_ne!(apart.flags & INTERVAL_EMPTY, 0);

    // An interval already marked empty is refused before anything.
    let mut marked = range(0, 100, INTERVAL_EMPTY);
    assert_eq!(refine_interval(&mut marked, &ANY), Err(Empty));

    // `refine_first`: the smallest value, open end kept only if it was.
    let mut wide = range(3, 9, INTERVAL_OPENMIN);
    assert_eq!(refine_first(&mut wide), Ok(true));
    assert_eq!((wide.min, wide.max), (3, 4));
    assert!(is_single(&wide));
    assert_eq!(value(&wide), 4, "the open minimum is left out");
}

#[test]
fn masks_refine_as_snd_mask_refine_does() {
    let mut both = Mask {
        bits: [0b1100, 0, 0, 0, 0, 0, 0, 0],
    };
    assert_eq!(refine_mask(&mut both, &only(2)), Ok(true));
    assert_eq!(both, only(2));
    assert_eq!(refine_mask(&mut both, &only(2)), Ok(false));
    assert_eq!(refine_mask(&mut both, &only(5)), Err(Empty));
    assert_eq!(only(255).bits[7], 1 << 31);
    assert_eq!(only(256).bits, [0; 8], "past the mask is nothing");
}

#[test]
fn hw_params_chooses_the_first_tick_time_and_keeps_the_rest() {
    let mut params = any();
    choose(&mut params, &VERSION_1.constraints()).expect("any");
    let tick = interval(&params, HW_PARAM_TICK_TIME);
    assert!(is_single(&tick));
    assert_eq!(value(&tick), 0);
    assert_eq!(value(&interval(&params, HW_PARAM_RATE)), 48_000);
    assert_eq!(params.info, INFO);
    assert_eq!(params.msbits, 16);
}
