/*
 * The ALSA numbers and layouts `libs/linux-abi/src/sound.rs` writes down,
 * printed from the UAPI header itself.
 *
 * Run by `libs/linux-abi/probe/sound.sh` on a Linux host with the kernel's
 * UAPI headers (`linux-libc-dev`) once natively for 64-bit and once for
 * ARMv7-A under qemu-arm, into `sound-64.txt` and `sound-32.txt` beside it.
 * The crate's tests read both files and require every line to match what the
 * crate says, so a number changed on either side fails `cargo test`.
 *
 * Each line is `name value`, in decimal. Layouts are `sizeof.<struct>` and
 * `offsetof.<struct>.<field>`, a nested field written with dots as C writes
 * it. The flags of `struct snd_interval` are bit-fields, which `offsetof`
 * cannot name, so they are printed as `bit.snd_interval.<flag>`: the word
 * that holds them with only that flag set, and that word's offset, found as
 * the first byte the flags change, as `offsetof.snd_interval.flags`.
 *
 * The 32-bit build is made with a 64-bit `time_t` (`_TIME_BITS=64`), the view
 * a musl or ferrousli program has: `asound.h` then defines
 * `__SND_STRUCT_TIME64`, which chooses the status and control structures,
 * the timestamps and the mmap offsets that `docs/AUDIO.md` §3.4 answers. The
 * time32 forms are not printed; the core never implements them.
 */

#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <time.h>
#include <sound/asound.h>

#define VALUE(name) printf("%s %llu\n", #name, (unsigned long long)(name))
#define SIZE(type) printf("sizeof.%s %zu\n", #type, sizeof(struct type))
#define FIELD(type, field) \
	printf("offsetof.%s.%s %zu\n", #type, #field, offsetof(struct type, field))

static void interval_bit(const char *name, void (*set)(struct snd_interval *))
{
	struct snd_interval interval;
	unsigned int word;

	memset(&interval, 0, sizeof interval);
	set(&interval);
	memcpy(&word, (const char *)&interval + 2 * sizeof(unsigned int), sizeof word);
	printf("bit.snd_interval.%s %u\n", name, word);
}

/* Where the flags' word is: the first byte a set flag changes, to its word. */
static void interval_flags(void)
{
	struct snd_interval interval;
	size_t at;

	memset(&interval, 0, sizeof interval);
	interval.openmin = interval.openmax = interval.integer = interval.empty = 1;
	for (at = 0; at < sizeof interval; at++)
		if (((const unsigned char *)&interval)[at])
			break;
	printf("offsetof.snd_interval.flags %zu\n", at - at % sizeof(unsigned int));
}

static void set_openmin(struct snd_interval *i) { i->openmin = 1; }
static void set_openmax(struct snd_interval *i) { i->openmax = 1; }
static void set_integer(struct snd_interval *i) { i->integer = 1; }
static void set_empty(struct snd_interval *i) { i->empty = 1; }

int main(void)
{
	/* The width of the view, and the protocol versions. */
	printf("sizeof.long %zu\n", sizeof(long));
	printf("sizeof.time_t %zu\n", sizeof(time_t));
	VALUE(SNDRV_PCM_VERSION);
	VALUE(SNDRV_CTL_VERSION);

	/* The PCM requests. */
	VALUE(SNDRV_PCM_IOCTL_PVERSION);
	VALUE(SNDRV_PCM_IOCTL_INFO);
	VALUE(SNDRV_PCM_IOCTL_TSTAMP);
	VALUE(SNDRV_PCM_IOCTL_TTSTAMP);
	VALUE(SNDRV_PCM_IOCTL_USER_PVERSION);
	VALUE(SNDRV_PCM_IOCTL_HW_REFINE);
	VALUE(SNDRV_PCM_IOCTL_HW_PARAMS);
	VALUE(SNDRV_PCM_IOCTL_HW_FREE);
	VALUE(SNDRV_PCM_IOCTL_SW_PARAMS);
	VALUE(SNDRV_PCM_IOCTL_STATUS);
	VALUE(SNDRV_PCM_IOCTL_DELAY);
	VALUE(SNDRV_PCM_IOCTL_HWSYNC);
	VALUE(SNDRV_PCM_IOCTL_SYNC_PTR);
	VALUE(SNDRV_PCM_IOCTL_STATUS_EXT);
	VALUE(SNDRV_PCM_IOCTL_CHANNEL_INFO);
	VALUE(SNDRV_PCM_IOCTL_PREPARE);
	VALUE(SNDRV_PCM_IOCTL_RESET);
	VALUE(SNDRV_PCM_IOCTL_START);
	VALUE(SNDRV_PCM_IOCTL_DROP);
	VALUE(SNDRV_PCM_IOCTL_DRAIN);
	VALUE(SNDRV_PCM_IOCTL_PAUSE);
	VALUE(SNDRV_PCM_IOCTL_REWIND);
	VALUE(SNDRV_PCM_IOCTL_RESUME);
	VALUE(SNDRV_PCM_IOCTL_XRUN);
	VALUE(SNDRV_PCM_IOCTL_FORWARD);
	VALUE(SNDRV_PCM_IOCTL_WRITEI_FRAMES);
	VALUE(SNDRV_PCM_IOCTL_READI_FRAMES);
	VALUE(SNDRV_PCM_IOCTL_WRITEN_FRAMES);
	VALUE(SNDRV_PCM_IOCTL_READN_FRAMES);
	VALUE(SNDRV_PCM_IOCTL_LINK);
	VALUE(SNDRV_PCM_IOCTL_UNLINK);

	/* The control requests. */
	VALUE(SNDRV_CTL_IOCTL_PVERSION);
	VALUE(SNDRV_CTL_IOCTL_CARD_INFO);
	VALUE(SNDRV_CTL_IOCTL_ELEM_LIST);
	VALUE(SNDRV_CTL_IOCTL_ELEM_INFO);
	VALUE(SNDRV_CTL_IOCTL_ELEM_READ);
	VALUE(SNDRV_CTL_IOCTL_ELEM_WRITE);
	VALUE(SNDRV_CTL_IOCTL_ELEM_LOCK);
	VALUE(SNDRV_CTL_IOCTL_ELEM_UNLOCK);
	VALUE(SNDRV_CTL_IOCTL_SUBSCRIBE_EVENTS);
	VALUE(SNDRV_CTL_IOCTL_ELEM_ADD);
	VALUE(SNDRV_CTL_IOCTL_ELEM_REPLACE);
	VALUE(SNDRV_CTL_IOCTL_ELEM_REMOVE);
	VALUE(SNDRV_CTL_IOCTL_TLV_READ);
	VALUE(SNDRV_CTL_IOCTL_TLV_WRITE);
	VALUE(SNDRV_CTL_IOCTL_TLV_COMMAND);
	VALUE(SNDRV_CTL_IOCTL_HWDEP_NEXT_DEVICE);
	VALUE(SNDRV_CTL_IOCTL_HWDEP_INFO);
	VALUE(SNDRV_CTL_IOCTL_PCM_NEXT_DEVICE);
	VALUE(SNDRV_CTL_IOCTL_PCM_INFO);
	VALUE(SNDRV_CTL_IOCTL_PCM_PREFER_SUBDEVICE);
	VALUE(SNDRV_CTL_IOCTL_RAWMIDI_NEXT_DEVICE);
	VALUE(SNDRV_CTL_IOCTL_RAWMIDI_INFO);
	VALUE(SNDRV_CTL_IOCTL_RAWMIDI_PREFER_SUBDEVICE);
	VALUE(SNDRV_CTL_IOCTL_UMP_NEXT_DEVICE);
	VALUE(SNDRV_CTL_IOCTL_POWER);
	VALUE(SNDRV_CTL_IOCTL_POWER_STATE);

	/* Streams, classes, access types, formats and subformats. */
	VALUE(SNDRV_PCM_STREAM_PLAYBACK);
	VALUE(SNDRV_PCM_STREAM_CAPTURE);
	VALUE(SNDRV_PCM_CLASS_GENERIC);
	VALUE(SNDRV_PCM_SUBCLASS_GENERIC_MIX);
	VALUE(SNDRV_PCM_ACCESS_MMAP_INTERLEAVED);
	VALUE(SNDRV_PCM_ACCESS_MMAP_NONINTERLEAVED);
	VALUE(SNDRV_PCM_ACCESS_MMAP_COMPLEX);
	VALUE(SNDRV_PCM_ACCESS_RW_INTERLEAVED);
	VALUE(SNDRV_PCM_ACCESS_RW_NONINTERLEAVED);
	VALUE(SNDRV_PCM_FORMAT_S8);
	VALUE(SNDRV_PCM_FORMAT_U8);
	VALUE(SNDRV_PCM_FORMAT_S16_LE);
	VALUE(SNDRV_PCM_FORMAT_U16_LE);
	VALUE(SNDRV_PCM_FORMAT_S32_LE);
	VALUE(SNDRV_PCM_FORMAT_U32_LE);
	VALUE(SNDRV_PCM_FORMAT_FLOAT_LE);
	VALUE(SNDRV_PCM_FORMAT_LAST);
	VALUE(SNDRV_PCM_SUBFORMAT_STD);
	VALUE(SNDRV_PCM_SUBFORMAT_LAST);

	/* The parameters of a refine, and the mask they index. */
	VALUE(SNDRV_PCM_HW_PARAM_ACCESS);
	VALUE(SNDRV_PCM_HW_PARAM_FORMAT);
	VALUE(SNDRV_PCM_HW_PARAM_SUBFORMAT);
	VALUE(SNDRV_PCM_HW_PARAM_FIRST_MASK);
	VALUE(SNDRV_PCM_HW_PARAM_LAST_MASK);
	VALUE(SNDRV_PCM_HW_PARAM_SAMPLE_BITS);
	VALUE(SNDRV_PCM_HW_PARAM_FRAME_BITS);
	VALUE(SNDRV_PCM_HW_PARAM_CHANNELS);
	VALUE(SNDRV_PCM_HW_PARAM_RATE);
	VALUE(SNDRV_PCM_HW_PARAM_PERIOD_TIME);
	VALUE(SNDRV_PCM_HW_PARAM_PERIOD_SIZE);
	VALUE(SNDRV_PCM_HW_PARAM_PERIOD_BYTES);
	VALUE(SNDRV_PCM_HW_PARAM_PERIODS);
	VALUE(SNDRV_PCM_HW_PARAM_BUFFER_TIME);
	VALUE(SNDRV_PCM_HW_PARAM_BUFFER_SIZE);
	VALUE(SNDRV_PCM_HW_PARAM_BUFFER_BYTES);
	VALUE(SNDRV_PCM_HW_PARAM_TICK_TIME);
	VALUE(SNDRV_PCM_HW_PARAM_FIRST_INTERVAL);
	VALUE(SNDRV_PCM_HW_PARAM_LAST_INTERVAL);
	VALUE(SNDRV_MASK_MAX);
	VALUE(SNDRV_PCM_HW_PARAMS_NORESAMPLE);
	VALUE(SNDRV_PCM_HW_PARAMS_EXPORT_BUFFER);
	VALUE(SNDRV_PCM_HW_PARAMS_NO_PERIOD_WAKEUP);
	VALUE(SNDRV_PCM_HW_PARAMS_NO_DRAIN_SILENCE);

	/* What a card says it can do, in `hw_params.info`. */
	VALUE(SNDRV_PCM_INFO_MMAP);
	VALUE(SNDRV_PCM_INFO_MMAP_VALID);
	VALUE(SNDRV_PCM_INFO_DOUBLE);
	VALUE(SNDRV_PCM_INFO_BATCH);
	VALUE(SNDRV_PCM_INFO_SYNC_APPLPTR);
	VALUE(SNDRV_PCM_INFO_PERFECT_DRAIN);
	VALUE(SNDRV_PCM_INFO_INTERLEAVED);
	VALUE(SNDRV_PCM_INFO_NONINTERLEAVED);
	VALUE(SNDRV_PCM_INFO_COMPLEX);
	VALUE(SNDRV_PCM_INFO_BLOCK_TRANSFER);
	VALUE(SNDRV_PCM_INFO_OVERRANGE);
	VALUE(SNDRV_PCM_INFO_RESUME);
	VALUE(SNDRV_PCM_INFO_PAUSE);
	VALUE(SNDRV_PCM_INFO_HALF_DUPLEX);
	VALUE(SNDRV_PCM_INFO_JOINT_DUPLEX);
	VALUE(SNDRV_PCM_INFO_SYNC_START);
	VALUE(SNDRV_PCM_INFO_NO_PERIOD_WAKEUP);
	VALUE(SNDRV_PCM_INFO_NO_REWINDS);

	/* States, timestamps, SYNC_PTR's flags and the mmap offsets. */
	VALUE(SNDRV_PCM_STATE_OPEN);
	VALUE(SNDRV_PCM_STATE_SETUP);
	VALUE(SNDRV_PCM_STATE_PREPARED);
	VALUE(SNDRV_PCM_STATE_RUNNING);
	VALUE(SNDRV_PCM_STATE_XRUN);
	VALUE(SNDRV_PCM_STATE_DRAINING);
	VALUE(SNDRV_PCM_STATE_PAUSED);
	VALUE(SNDRV_PCM_STATE_SUSPENDED);
	VALUE(SNDRV_PCM_STATE_DISCONNECTED);
	VALUE(SNDRV_PCM_TSTAMP_NONE);
	VALUE(SNDRV_PCM_TSTAMP_ENABLE);
	VALUE(SNDRV_PCM_TSTAMP_TYPE_GETTIMEOFDAY);
	VALUE(SNDRV_PCM_TSTAMP_TYPE_MONOTONIC);
	VALUE(SNDRV_PCM_TSTAMP_TYPE_MONOTONIC_RAW);
	VALUE(SNDRV_PCM_SYNC_PTR_HWSYNC);
	VALUE(SNDRV_PCM_SYNC_PTR_APPL);
	VALUE(SNDRV_PCM_SYNC_PTR_AVAIL_MIN);
	VALUE(SNDRV_PCM_MMAP_OFFSET_DATA);
	VALUE(SNDRV_PCM_MMAP_OFFSET_STATUS);
	VALUE(SNDRV_PCM_MMAP_OFFSET_CONTROL);
	VALUE(SNDRV_PCM_MMAP_OFFSET_STATUS_OLD);
	VALUE(SNDRV_PCM_MMAP_OFFSET_CONTROL_OLD);
	VALUE(SNDRV_PCM_MMAP_OFFSET_STATUS_NEW);
	VALUE(SNDRV_PCM_MMAP_OFFSET_CONTROL_NEW);

	/* Layouts: the refine's parts. */
	SIZE(snd_interval);
	FIELD(snd_interval, min);
	FIELD(snd_interval, max);
	interval_flags();
	interval_bit("openmin", set_openmin);
	interval_bit("openmax", set_openmax);
	interval_bit("integer", set_integer);
	interval_bit("empty", set_empty);
	SIZE(snd_mask);

	SIZE(snd_pcm_hw_params);
	FIELD(snd_pcm_hw_params, flags);
	FIELD(snd_pcm_hw_params, masks);
	FIELD(snd_pcm_hw_params, mres);
	FIELD(snd_pcm_hw_params, intervals);
	FIELD(snd_pcm_hw_params, ires);
	FIELD(snd_pcm_hw_params, rmask);
	FIELD(snd_pcm_hw_params, cmask);
	FIELD(snd_pcm_hw_params, info);
	FIELD(snd_pcm_hw_params, msbits);
	FIELD(snd_pcm_hw_params, rate_num);
	FIELD(snd_pcm_hw_params, rate_den);
	FIELD(snd_pcm_hw_params, fifo_size);
	FIELD(snd_pcm_hw_params, sync);
	FIELD(snd_pcm_hw_params, reserved);

	SIZE(snd_pcm_sw_params);
	FIELD(snd_pcm_sw_params, tstamp_mode);
	FIELD(snd_pcm_sw_params, period_step);
	FIELD(snd_pcm_sw_params, sleep_min);
	FIELD(snd_pcm_sw_params, avail_min);
	FIELD(snd_pcm_sw_params, xfer_align);
	FIELD(snd_pcm_sw_params, start_threshold);
	FIELD(snd_pcm_sw_params, stop_threshold);
	FIELD(snd_pcm_sw_params, silence_threshold);
	FIELD(snd_pcm_sw_params, silence_size);
	FIELD(snd_pcm_sw_params, boundary);
	FIELD(snd_pcm_sw_params, proto);
	FIELD(snd_pcm_sw_params, tstamp_type);
	FIELD(snd_pcm_sw_params, reserved);

	/* Status: the libc's `struct timespec` in it. */
	SIZE(timespec);
	FIELD(timespec, tv_sec);
	FIELD(timespec, tv_nsec);
	SIZE(snd_pcm_status);
	FIELD(snd_pcm_status, state);
	FIELD(snd_pcm_status, trigger_tstamp);
	FIELD(snd_pcm_status, tstamp);
	FIELD(snd_pcm_status, appl_ptr);
	FIELD(snd_pcm_status, hw_ptr);
	FIELD(snd_pcm_status, delay);
	FIELD(snd_pcm_status, avail);
	FIELD(snd_pcm_status, avail_max);
	FIELD(snd_pcm_status, overrange);
	FIELD(snd_pcm_status, suspended_state);
	FIELD(snd_pcm_status, audio_tstamp_data);
	FIELD(snd_pcm_status, audio_tstamp);
	FIELD(snd_pcm_status, driver_tstamp);
	FIELD(snd_pcm_status, audio_tstamp_accuracy);
	FIELD(snd_pcm_status, reserved);

	/* SYNC_PTR, whose halves are the mapped pages' layouts. */
	SIZE(snd_pcm_mmap_status);
	FIELD(snd_pcm_mmap_status, state);
	FIELD(snd_pcm_mmap_status, hw_ptr);
	FIELD(snd_pcm_mmap_status, tstamp);
	FIELD(snd_pcm_mmap_status, suspended_state);
	FIELD(snd_pcm_mmap_status, audio_tstamp);
	SIZE(snd_pcm_mmap_control);
	FIELD(snd_pcm_mmap_control, appl_ptr);
	FIELD(snd_pcm_mmap_control, avail_min);
	SIZE(snd_pcm_sync_ptr);
	FIELD(snd_pcm_sync_ptr, flags);
	FIELD(snd_pcm_sync_ptr, s);
	FIELD(snd_pcm_sync_ptr, s.status.state);
	FIELD(snd_pcm_sync_ptr, s.status.hw_ptr);
	FIELD(snd_pcm_sync_ptr, s.status.tstamp);
	FIELD(snd_pcm_sync_ptr, s.status.tstamp.tv_sec);
	FIELD(snd_pcm_sync_ptr, s.status.tstamp.tv_nsec);
	FIELD(snd_pcm_sync_ptr, s.status.suspended_state);
	FIELD(snd_pcm_sync_ptr, s.status.audio_tstamp);
	FIELD(snd_pcm_sync_ptr, c);
	FIELD(snd_pcm_sync_ptr, c.control.appl_ptr);
	FIELD(snd_pcm_sync_ptr, c.control.avail_min);

	/* A transfer, and the channel of mapped access. */
	SIZE(snd_xferi);
	FIELD(snd_xferi, result);
	FIELD(snd_xferi, buf);
	FIELD(snd_xferi, frames);
	SIZE(snd_pcm_channel_info);

	/* What the nodes say they are. */
	SIZE(snd_pcm_info);
	FIELD(snd_pcm_info, device);
	FIELD(snd_pcm_info, subdevice);
	FIELD(snd_pcm_info, stream);
	FIELD(snd_pcm_info, card);
	FIELD(snd_pcm_info, id);
	FIELD(snd_pcm_info, name);
	FIELD(snd_pcm_info, subname);
	FIELD(snd_pcm_info, dev_class);
	FIELD(snd_pcm_info, dev_subclass);
	FIELD(snd_pcm_info, subdevices_count);
	FIELD(snd_pcm_info, subdevices_avail);
	FIELD(snd_pcm_info, pad1);
	FIELD(snd_pcm_info, reserved);

	SIZE(snd_ctl_card_info);
	FIELD(snd_ctl_card_info, card);
	FIELD(snd_ctl_card_info, pad);
	FIELD(snd_ctl_card_info, id);
	FIELD(snd_ctl_card_info, driver);
	FIELD(snd_ctl_card_info, name);
	FIELD(snd_ctl_card_info, longname);
	FIELD(snd_ctl_card_info, reserved_);
	FIELD(snd_ctl_card_info, mixername);
	FIELD(snd_ctl_card_info, components);

	SIZE(snd_ctl_elem_id);
	SIZE(snd_ctl_elem_list);
	FIELD(snd_ctl_elem_list, offset);
	FIELD(snd_ctl_elem_list, space);
	FIELD(snd_ctl_elem_list, used);
	FIELD(snd_ctl_elem_list, count);
	FIELD(snd_ctl_elem_list, pids);
	FIELD(snd_ctl_elem_list, reserved);
	return 0;
}
