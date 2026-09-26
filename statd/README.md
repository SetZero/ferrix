# ferrix-statd

Ferrix's stat service: a program like any other on Ferrix that reads what
Linux's `vmstat` and `top` read (`/proc/stat`, `/proc/meminfo`,
`/proc/uptime`, `/proc/<pid>/stat`) and writes one line of JSON a sample to
its standard output:

```
FERRIX-STAT-START {"version":1,"interval_ms":500,"seconds":20,"cpus":8,"mem_total_kib":7752680,"pid":1}
FERRIX-STAT {"t":41.33,"all":0.0,"cpu":[0.0,…],"mem":{"total":…,"free":…,"available":…,"cached":…,"buffers":…},
             "rate":{"irq":421,"ctxt":1041,"forks":0.0},
             "tasks":{"processes":2,"threads":2,"running":1,"blocked":0,"sleeping":1},
             "top":[{"pid":1,"comm":"ferrix-statd","cpu":0.0,"rss_kib":708,"threads":1},…]}
FERRIX-STAT-END {"samples":40,"seconds":20.0}
```

`t` is Ferrix's uptime in seconds; `cpu` is each processor's load since the
last sample and `all` all of them together, in percent; `mem` is in KiB;
`rate` is interrupts, context switches and forks a second; `top` is the five
processes that used the most processor time since the last sample, as a
percentage of one processor.

As pid 1 its output is the console: a crosvm guest's 16550, which
`tools/pixel7-monitor` reads live, or the Pixel 7's `ramoops` record, which the
monitor reads once Android is back. The boot console on the phone's screen
draws only the kernel's own lines, so these do not fill it.

## Running it

`cargo xtask build --statd` puts it in an image at `/sbin/ferrix-statd`, and
the kernel starts it after the boot checks when the command line says

```
ferrix.init=/sbin/ferrix-statd ferrix.statd.interval=500 ferrix.statd.seconds=20
```

It reads both settings from `/proc/cmdline`. `interval` is in milliseconds,
100 at least; `seconds` 0, the default, runs until the machine is stopped.
When it ends, pid 1 has exited and the kernel powers off, which on the phone
is the watchdog's reset back to Android.

* **A crosvm guest on the phone**: the monitor's "Stats" choice, or
  `crosvm run -p ferrix.init=/sbin/ferrix-statd -p ferrix.statd.seconds=0 …`;
  the loader passes every `ferrix.*` word of crosvm's `bootargs` on.
* **A native boot on the phone**: the monitor's "Boot Ferrix" with "Stats"
  set, or the helper's `POST /boot?stats=<seconds>`. The helper boots a copy
  of the image with the options in its boot image header, which ABL puts in
  the device tree's `bootargs`; the loader hands every `ferrix.*` word of
  those to the kernel. The options can also be compiled into the loader,
  with `FERRIX_PIXEL7_CMDLINE_EXTRA="…" $P/build-run.sh <name>`. Either way
  give it a number of seconds: the kernel feeds the watchdog while it runs,
  so a service that never ends keeps the phone in Ferrix. Nothing reaches the
  PC while Ferrix runs, because it has no USB, so the graphs come from the
  `ramoops` record once Android is back.
* **QEMU**: `cargo xtask test-boot --arch aarch64 --statd --kernel-option
  ferrix.init=/sbin/ferrix-statd --kernel-option ferrix.statd.seconds=3`.

It is built like zinc, with std, statically against the target's own musl,
from a workspace of its own (`xtask/src/statd.rs`), and has no dependencies.
