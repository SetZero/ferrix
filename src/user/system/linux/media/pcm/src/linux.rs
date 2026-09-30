//! The ioctls, as `src/user/linux/compositor/tone` makes them.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

use ferrix_linux_abi::socket::Width;
use ferrix_linux_abi::sound::{
    ACCESS_RW_INTERLEAVED, FORMAT_S16_LE, HW_PARAM_BUFFER_SIZE, HW_PARAM_CHANNELS,
    HW_PARAM_FIRST_INTERVAL, HW_PARAM_PERIOD_SIZE, HW_PARAM_RATE, HwParams, Interval, Mask,
    PCM_VERSION, Pcm, SUBFORMAT_STD, SwParams, TSTAMP_TYPE_MONOTONIC, Xferi, protocol_version,
};

use crate::{CHANNELS, RATE};

const fn width() -> Width {
    if usize::BITS == 32 {
        Width::Bits32
    } else {
        Width::Bits64
    }
}

/// What the card chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Frames a second.
    pub rate: u32,
    /// Channels in a frame.
    pub channels: u32,
    /// Frames between wake-ups.
    pub period: u32,
    /// Frames the card holds.
    pub buffer: u32,
}

/// An open, prepared playback stream.
#[derive(Debug)]
pub struct Playback {
    pcm: OwnedFd,
    config: Config,
    written: u64,
    underruns: u32,
}

fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn fail(what: &str, answer: i32) -> io::Error {
    io::Error::other(format!("{what}: errno {}", -answer))
}

impl Playback {
    /// Open `/dev/snd/pcmC0D0p`, set it to 48 kHz stereo `S16_LE`, and
    /// prepare it. Playing starts once `start(config)` frames are queued, at
    /// least one and at most the buffer: a player that wants to start at
    /// once answers a period, one that wants a cushion answers more.
    ///
    /// # Errors
    ///
    /// No card, or one that refused the format.
    pub fn open(start: impl FnOnce(Config) -> u32) -> io::Result<Self> {
        let path = CString::new("/dev/snd/pcmC0D0p").map_err(io::Error::other)?;
        // SAFETY: a NUL-terminated path that lives across the call.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` was just opened and nothing else owns it.
        let pcm = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut playback = Self {
            pcm,
            config: Config {
                rate: 0,
                channels: 0,
                period: 0,
                buffer: 0,
            },
            written: 0,
            underruns: 0,
        };
        playback.config = playback.hardware()?;
        let threshold = start(playback.config);
        playback.software(threshold.clamp(1, playback.config.buffer))?;
        playback.prepare()?;
        Ok(playback)
    }

    /// What the card chose.
    #[must_use]
    pub const fn config(&self) -> Config {
        self.config
    }

    /// Underruns so far.
    #[must_use]
    pub const fn underruns(&self) -> u32 {
        self.underruns
    }

    /// Frames written so far.
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }

    fn ioctl(&self, request: u32, bytes: &mut [u8]) -> i32 {
        // SAFETY: `bytes` is as long as the request's argument, which every
        // caller sizes from the request's own layout, and lives across it.
        let answer = unsafe { libc::ioctl(self.pcm.as_raw_fd(), request as _, bytes.as_mut_ptr()) };
        if answer < 0 { -errno() } else { answer }
    }

    fn int(&self, request: Pcm, value: i32) -> (i32, i32) {
        let mut bytes = value.to_le_bytes();
        let answer = self.ioctl(request.request(width()), &mut bytes);
        (answer, i32::from_le_bytes(bytes))
    }

    fn hw(&self, request: Pcm, params: &mut HwParams) -> i32 {
        let mut bytes = vec![0_u8; HwParams::size(width())];
        let _ = params.write(width(), &mut bytes);
        let answer = self.ioctl(request.request(width()), &mut bytes);
        if answer == 0
            && let Some(read) = HwParams::read(width(), &bytes)
        {
            *params = read;
        }
        answer
    }

    fn hardware(&self) -> io::Result<Config> {
        let (_, version) = self.int(Pcm::Pversion, 0);
        if version as u32 != PCM_VERSION {
            return Err(io::Error::other(format!("PVERSION {version:#x}")));
        }
        let _ = self.int(Pcm::UserPversion, protocol_version(2, 0, 18) as i32);
        let _ = self.int(Pcm::Ttstamp, TSTAMP_TYPE_MONOTONIC as i32);
        let mut params = any();
        params.masks[0] = only(ACCESS_RW_INTERLEAVED);
        params.masks[1] = only(FORMAT_S16_LE);
        params.masks[2] = only(SUBFORMAT_STD);
        let exactly = |value: u32| Interval {
            min: value,
            max: value,
            flags: 0,
        };
        set(&mut params, HW_PARAM_RATE, exactly(RATE));
        set(&mut params, HW_PARAM_CHANNELS, exactly(CHANNELS as u32));
        // Refine first, as `snd_pcm_set_params` does, so the parameters set
        // are ones the card has already narrowed.
        let answer = self.hw(Pcm::HwRefine, &mut params);
        if answer != 0 {
            return Err(fail("HW_REFINE", answer));
        }
        let answer = self.hw(Pcm::HwParams, &mut params);
        if answer != 0 {
            return Err(fail("HW_PARAMS", answer));
        }
        let at = |param: u32| {
            params
                .intervals
                .get((param - HW_PARAM_FIRST_INTERVAL) as usize)
                .map_or(0, |interval| interval.min)
        };
        Ok(Config {
            rate: at(HW_PARAM_RATE),
            channels: at(HW_PARAM_CHANNELS),
            period: at(HW_PARAM_PERIOD_SIZE),
            buffer: at(HW_PARAM_BUFFER_SIZE),
        })
    }

    fn software(&self, start: u32) -> io::Result<()> {
        let params = SwParams {
            tstamp_mode: 0,
            period_step: 1,
            sleep_min: 0,
            avail_min: u64::from(self.config.period),
            xfer_align: 1,
            start_threshold: u64::from(start),
            stop_threshold: u64::from(self.config.buffer),
            silence_threshold: 0,
            silence_size: 0,
            boundary: 0,
            proto: PCM_VERSION,
            tstamp_type: TSTAMP_TYPE_MONOTONIC,
            reserved: [0; 56],
        };
        let mut bytes = vec![0_u8; SwParams::size(width())];
        let _ = params.write(width(), &mut bytes);
        let answer = self.ioctl(Pcm::SwParams.request(width()), &mut bytes);
        if answer == 0 {
            Ok(())
        } else {
            Err(fail("SW_PARAMS", answer))
        }
    }

    fn prepare(&self) -> io::Result<()> {
        // SAFETY: a request that takes no argument.
        let answer =
            unsafe { libc::ioctl(self.pcm.as_raw_fd(), Pcm::Prepare.request(width()) as _, 0) };
        if answer < 0 {
            Err(fail("PREPARE", -errno()))
        } else {
            Ok(())
        }
    }

    /// Write interleaved stereo frames, waiting while the card's buffer is
    /// full. An underrun prepares the stream again and carries on.
    ///
    /// # Errors
    ///
    /// Anything but an underrun: the card went away, or refused the write.
    pub fn write(&mut self, samples: &[i16]) -> io::Result<()> {
        let frames = samples.len() / CHANNELS;
        let mut sent = 0;
        while sent < frames {
            let rest = samples.get(sent * CHANNELS..).unwrap_or(&[]);
            let xferi = Xferi {
                result: 0,
                buf: rest.as_ptr() as u64,
                frames: (frames - sent) as u64,
            };
            let mut bytes = vec![0_u8; Xferi::size(width())];
            let _ = xferi.write(width(), &mut bytes);
            let answer = self.ioctl(Pcm::WriteiFrames.request(width()), &mut bytes);
            if answer == -libc::EPIPE {
                self.underruns += 1;
                self.recover()?;
                continue;
            }
            if answer == -libc::EINTR {
                continue;
            }
            let result = Xferi::read(width(), &bytes).map_or(-1, |x| x.result);
            if answer != 0 || result <= 0 {
                return Err(fail("WRITEI_FRAMES", answer.min(-1)));
            }
            let done = result as usize;
            sent += done;
            self.written += done as u64;
        }
        Ok(())
    }

    /// After an underrun: what was queued is gone, so the speaker is where
    /// the program is.
    fn recover(&mut self) -> io::Result<()> {
        self.prepare()
    }

    /// Frames the speaker has played: written, less what is queued.
    ///
    /// # Errors
    ///
    /// The card went away.
    pub fn played(&mut self) -> io::Result<u64> {
        let mut bytes = [0_u8; 8];
        let size = if usize::BITS == 32 { 4 } else { 8 };
        let answer = self.ioctl(
            Pcm::Delay.request(width()),
            bytes.get_mut(..size).unwrap_or(&mut []),
        );
        if answer == -libc::EPIPE {
            self.underruns += 1;
            self.recover()?;
            return Ok(self.written);
        }
        if answer != 0 {
            return Err(fail("DELAY", answer));
        }
        let delay = if size == 4 {
            let mut word = [0_u8; 4];
            word.copy_from_slice(bytes.get(..4).unwrap_or(&[0; 4]));
            u64::try_from(i32::from_le_bytes(word)).unwrap_or(0)
        } else {
            u64::try_from(i64::from_le_bytes(bytes)).unwrap_or(0)
        };
        Ok(self.written.saturating_sub(delay))
    }

    /// Play out what is queued and stop.
    ///
    /// # Errors
    ///
    /// The card went away.
    pub fn drain(&mut self) -> io::Result<()> {
        // SAFETY: a request that takes no argument.
        let answer =
            unsafe { libc::ioctl(self.pcm.as_raw_fd(), Pcm::Drain.request(width()) as _, 0) };
        if answer < 0 && errno() != libc::EPIPE {
            Err(fail("DRAIN", -errno()))
        } else {
            Ok(())
        }
    }
}

/// What `snd_pcm_hw_params_any` sends: every mask full, every interval
/// everything.
fn any() -> HwParams {
    HwParams {
        flags: 0,
        masks: [Mask {
            bits: [u32::MAX; 8],
        }; 3],
        mres: [Mask { bits: [0; 8] }; 5],
        intervals: [Interval {
            min: 0,
            max: u32::MAX,
            flags: 0,
        }; 12],
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

fn set(params: &mut HwParams, param: u32, value: Interval) {
    if let Some(interval) = params
        .intervals
        .get_mut((param - HW_PARAM_FIRST_INTERVAL) as usize)
    {
        *interval = value;
    }
}

fn only(bit: u32) -> Mask {
    let mut bits = [0_u32; 8];
    if let Some(word) = bits.get_mut((bit / 32) as usize) {
        *word = 1 << (bit % 32);
    }
    Mask { bits }
}
