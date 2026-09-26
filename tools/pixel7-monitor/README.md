# Pixel 7 Monitor

A desktop window on the Pixel 7 while it runs Ferrix. It needs the phone on
USB with adb authorised, and nothing else: no Claude, no terminal.

* **Where the phone is**: Android, fastboot, away in Ferrix (gone from USB
  after a `fastboot boot`), or not connected, with a strip along the top
  that colours the last ten minutes by state.
* **Boot Ferrix** natively, through the launcher's helper
  (`bootloaders/pixel7/launcher/helper.py`). If the helper is not running,
  the monitor starts it. While the phone is away the boot card follows the
  helper's phase, and when Android is back it loads the run's `ramoops`
  record.
* **Run Ferrix in a VM** on the phone's own crosvm, with the vCPUs, RAM and
  stat service chosen in the header. The console streams live, and pause, resume and stop
  go to crosvm's control socket. Each guest's console is kept as
  `~/.local/share/ferrix/pixel7/vm-<time>/run.log`.
* **The boot card**: stages 1 to 12, each with the time it was reached in a
  live run, and the `FERRIX-BOOT-OK` line, or the `FERRIX-PANIC` line and its
  `FX-` code.
* **The console**: follows the newest line while the reader is within ten
  lines of the end. It has a filter that highlights matches.
* **Graphs, once a second, fifteen minutes kept**:
  * **CPU**: load, overall and per cluster (LITTLE, MID, BIG), with a bar
    for each of the eight cores.
  * **Frequency**: each cluster's.
  * **Memory**: used, cached and swap.
  * **Temperature**: the BIG, MID, LITTLE, GPU (G3D), TPU, battery and skin
    sensors.
  * **GPU and memory bus**: their frequencies.
  * **Battery**: draw and voltage.
  * **crosvm**: processor use and resident memory while a guest runs.
* **Ferrix's own stats**, on the graphs' second tab, from `ferrix-statd`
  (`statd/`), Ferrix's stat service:
  * **CPU**: load, overall and per processor.
  * **Memory**: used and cached.
  * **Rates**: interrupts and context switches a second.
  * **Tasks**: processes, threads and runnable tasks.
  * **Busiest processes**: a table of the top five.

  A VM run with "Stats" on starts it as pid 1 and the tab fills live. A
  native boot with "Stats" on runs it for that long ("until stopped" is a
  minute there). The phone is off USB meanwhile, since Ferrix has no USB,
  and its samples are in the `ramoops` record. The monitor loads that record
  when Android is back and graphs the whole run. The console
  hides the `FERRIX-STAT` lines unless "stat lines" is ticked.
* **Runs**: every `run.log` under `~/.local/share/ferrix/pixel7`, newest
  first, with its result. Clicking one loads it into the console.

The stats come from one shell script a second, fed to a shell on the phone
that stays open: a root shell where Magisk allows it, so one prompt rather
than one a second. Temperatures, the GPU and the memory bus need root. The
rest is read without it. Nothing on the phone is written except the VM's
socket and image, in `/data/local/tmp/ferrix-vm`.

## Building and running

Tauri 2, with a plain HTML, CSS and JavaScript front end and no bundler or
npm packages; the charts are its own, on a canvas (`ui/chart.js`). It is a
Cargo workspace of its own, outside the directories Ferrix's gates read.

```sh
cd tools/pixel7-monitor
cargo run --release            # or: cargo tauri dev
```

On nazuna this needs WebKitGTK 4.1, which is installed, and adb and fastboot
on `PATH`. `PIXEL7_SERIAL` picks another phone than `28171FDH2001RC`.
