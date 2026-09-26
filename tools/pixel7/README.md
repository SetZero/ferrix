# Pixel 7 tools

What runs Ferrix on the Pixel 7 and watches it, from the phone and from the
PC. None of it is part of Ferrix's own image. Each part builds apart from the
Cargo workspace and the gates.

| Where | What |
|---|---|
| `android/` | The Android app, "Boot Ferrix" (`dev.ferrix.launcher`): Kotlin and Compose, a Gradle project of its own |
| `helper.py` | The helper on the PC that the app's Boot button and the monitor ask to boot the phone |
| `monitor/` | The desktop monitor, "Pixel 7 · Ferrix": Tauri 2, a Cargo workspace of its own (`monitor/README.md`) |

The loader these boot, and the phone's state, are in `boot/pixel7`
(`HANDOVER.md` there).

## The app and the helper

An Android app with two ways to run Ferrix on the Pixel 7, and the helper on
the PC that the first one needs:

* **Boot Ferrix** reboots the phone into Ferrix, through the PC.
* **Run in a VM** runs Ferrix beside Android, as a guest of the phone's own
  KVM through Android's crosvm. No PC and no reboot are needed, and it takes
  about 6 seconds to `FERRIX-BOOT-OK`.

The phone cannot start Ferrix by itself. Its kernel has no `kexec`, and
nothing may be flashed (`boot/pixel7/HANDOVER.md`, "Never write anything that survives
a reset"). What starts Ferrix is `fastboot boot` from a PC, so the button asks
the PC. The phone has to be plugged into it, and nothing on the phone is
changed: Ferrix runs from RAM, and its watchdog brings Android back about 75
seconds later.

```
phone: Boot Ferrix ──HTTP to 127.0.0.1:47707──▶ adb reverse ──USB──▶ helper.py on the PC
helper: adb reboot bootloader → fastboot stage vendor_boot.img → fastboot boot boot.img
        → wait for Android → save the ramoops record → report FERRIX-BOOT-OK or the panic
```

### The helper

```sh
python3 tools/pixel7/helper.py              # the newest $P/*/boot.img
python3 tools/pixel7/helper.py --image PATH # a given one
```

`P` is `~/.local/share/ferrix/pixel7`, where `vendor_boot.img` and the run
directories are. The helper listens on 127.0.0.1 only and keeps the phone's
`adb reverse` pointing at it; a reboot drops it, and it is put back. Each run's
record lands in `$P/launcher-<time>/run.log`. The endpoints are `GET /status`
and `POST /boot`. Build an image first with `$P/build-run.sh`, as the
handover's "Running the phone with nobody there" says.

### The app

Kotlin and Jetpack Compose, Material 3 with the wallpaper's dynamic colours.
The Gradle project here builds offline from the cache PhoneLink's build left,
with the SDK in `~/Android/Sdk` and a JDK that has `javac`:

```sh
cd tools/pixel7/android
JAVA_HOME=~/Android/jdk/jdk-21.0.12.1+1 ANDROID_HOME=~/Android/Sdk \
    ~/.gradle/wrapper/dists/gradle-9.8.0-bin/*/gradle-9.8.0/bin/gradle \
    --offline -Dorg.gradle.java.installations.paths=$HOME/Android/jdk/jdk-21.0.12.1+1 \
    assembleDebug
adb -s 28171FDH2001RC install -r app/build/outputs/apk/debug/app-debug.apk
```

The system JDK on nazuna is a runtime with no `javac`, which Gradle's Java
compile step needs even in a Kotlin-only app, so `JAVA_HOME` has to name the
SDK's JDK. The app is `dev.ferrix.launcher`, labelled "Boot Ferrix".

Any app on the phone with the internet permission could reach the helper's
port while the cable is in, and all it could do is what the button does.

### The VM

The app runs, through `su` (Magisk asks once), after starting the screen's
bridge (below):

```sh
cd /data/local/tmp/ferrix-vm && /apex/com.android.virt/bin/crosvm run \
    --disable-sandbox -m 4096 --cpus 8 -s crosvm.sock --serial type=stdout,num=1,stdin \
    --gpu 'backend=2d,displays=[[mode=windowed[1080,2400]]]' --android-display-service cid:70000 \
    --input 'single-touch[path=touch.sock,width=1080,height=2400]' \
    --input 'keyboard[path=keyboard.sock]' --input 'mouse[path=mouse.sock]' ferrix.Image
```

With `desktop.Image` beside it, the app boots that instead, with
`-p ferrix.checks=skip`; with `chromium.img` too
(`scripts/fetch/fetch-chromium-arm64.sh`, pushed by
`build-desktop.sh --chrome --push`), it adds three blank 1 MiB disks and then
the volume, so that the volume is `vdd`, the first disk Ferrix mounts at
`/data`.

The 16550 takes crosvm's standard input too, the su process's, which the app
keeps open for the run: a line it writes there reaches the guest's shell on
its console.

`ferrix.Image` is the raw loader `Image` from the helper's run directory, the
same loader and kernel `fastboot boot` gets, which the helper pushes whenever
the phone's copy differs. It needs to have been built with guest support
(`9df3769b` or later). The loader finds it is a guest, entered at EL1 with
crosvm's 16550 as `stdout-path`, and sends its log and the kernel's there.
crosvm's machine has 2 to 8 vCPUs, GICv3, PSCI through `hvc`, and RAM at
`0x8000_0000`, a virtio-gpu, and virtio-input single-touch, keyboard and
mouse devices, which Ferrix drives since its kernel reads crosvm's `pci-host-cam-generic` host
and gives each function the INTx line its `interrupt-map` names. The guest
powers off at the end of its run, and crosvm exits.

**The desktop.** When `desktop.Image` is beside `ferrix.Image`, the app boots
it instead: the loader wrapping a kernel and initramfs whose init is the
compositor. `tools/pixel7/build-desktop.sh <dir> [scale] [--push]` builds it
(`cargo xtask flash --compositor --size 1080x2400 --scale 2 --layout us
--wallpaper none`, then the loader) and with `--push` copies it to the phone.
The scale is 2 unless given, since 1080x2400 at 1 is text a few millimetres
high, and the keymap US, which the app's key codes are. Delete
`desktop.Image` to go back to the console image.

Starting a VM opens it full screen: one bar, with the run's state and a menu
to pause or resume it, stop it, run it again, or go back, and the console
under it. The console follows the newest line while the reader is within ten
lines of the end. Pause and stop go to crosvm's control socket
(`crosvm suspend|resume|stop /data/local/tmp/ferrix-vm/crosvm.sock`). A
paused guest's counter keeps running, so pausing during the boot's
self-checks can fail a timing check, as it did once in stage 3 (FX-0302).

### The screen

The guest's display is the phone's, upright: its full size in pixels
(1080×2400 on this Pixel 7), measured when the VM starts. Once it is there,
the VM opens on it, with the menu floating in a corner, and the menu switches
between the screen and the console. How it gets there, all of it tried on
this phone (Android 17 CP3A.260905.009, Magisk):

1. With `--gpu … --android-display-service cid:70000`, crosvm makes a
   binder, `android.crosvm.ICrosvmAndroidDisplayService`, and gives it to
   `android.system.virtualizationservice` (`setDisplayService`) under that
   cid, and virtualizationservice hands it on to root only. Before
   CP3A.260905.009 the name was free and the service kept one binder for
   everyone; now crosvm refuses a name without `cid:`. 70000 is above the
   cids virtualizationservice gives its own VMs.
2. So a root process fetches it: the app's own `Bridge` class, run by the su
   script as `CLASSPATH=<this APK> app_process /system/bin
   dev.ferrix.launcher.Bridge …`, which calls
   `IVirtualizationServiceInternal.waitDisplayService(70000)` by its
   transaction code, 17, the one the Terminal app's own inlined call uses
   (the Terminal app's APK no longer carries callable stubs). The service
   keeps the last binder it was given even after that crosvm died, so the
   bridge takes only one that answers a ping and is not the one held
   before this run's crosvm started.
3. The bridge hands it to the app through the app's exported provider
   (`GuestLink`, which takes calls from root only and for the current run's
   token), by the activity manager's `getContentProviderExternal` and the
   provider's `call`.
4. The app calls the binder itself, which works from an untrusted app:
   `setSurface` (transaction 1) with its SurfaceView's surface when it has
   one, `saveFrameForSurface` (4) and `removeSurface` (3) when it goes, and
   `drawSavedFrameForSurface` (5) when it is back. Each takes `forCursor`
   last.
5. The guest's compositor puts its pointer on the card's cursor plane, which
   crosvm draws into a second surface, as AOSP's Terminal app has it: a
   64×64 RGBA SurfaceView over the screen, lent with `setSurface(…,
   forCursor = true)`. `setCursorStream` (2) takes one end of a socket pair,
   on which crosvm writes the cursor's position as `(x: i32, y: i32)`
   little-endian; the app reads the other end and moves the cursor's surface,
   made a child of the screen's `SurfaceControl`, there.
6. crosvm's `--input <type>[path=…]` connects to a Unix socket at that path as
   it starts, and will not start if nothing listens there ("failed to open
   event device"). The bridge listens on `touch.sock`, `keyboard.sock` and
   `mouse.sock` first, each made under another name and renamed into place
   once it listens, and the script starts crosvm once all three are there.
   If they are not there within ten seconds, the guest runs with its console
   only.
7. The bridge also hands the app a binder of its own, which takes batches of
   virtio_input_events (8 bytes, little-endian: `u16 type, u16 code, u32
   value`), a transaction for each device (touch, keyboard, mouse, from
   `FIRST_CALL_TRANSACTION`), and writes them to crosvm's connection for it.

The bridge ends when crosvm does, by its process or a connection, so
stopping the VM stops it too. It prints its steps as `FERRIX-VM-BRIDGE` lines
among the console's.

### Controls

A finger on the screen is what the menu says, touch or trackpad, as Microsoft's
Remote Desktop app has them; the app remembers the choice.

- **Touch.** The pointer goes where the finger lands (`ABS_X`/`ABS_Y`, no
  press); moving presses `BTN_TOUCH` at the landing point and drags, lifting
  unmoved is a tap. Held still (Android's long-press time) is the mouse's
  `BTN_RIGHT`.
- **Trackpad.** A finger moves the pointer by how far it goes (`REL_X`/`REL_Y`,
  1.2 guest pixels a pixel, up to three times that for a quick finger); a tap
  is `BTN_LEFT`; held still first, it drags with `BTN_LEFT` down.
- **Both.** Two fingers moving are the wheel (`REL_WHEEL`, a click each 12 dp,
  content following the fingers); two fingers tapped are `BTN_RIGHT`.

The keyboard button beside the menu opens the soft keyboard, the view being
an editor for a visible password (no suggestions, no composing), and a row
over it: Esc, Tab, Ctrl, Alt, Super, the arrows, `|`, `/` and `-`. Ctrl, Alt
and Super are sticky: tapped, they hold the next key; tapped twice, until
tapped again. Text becomes key presses for a US keymap, the guest's, with
Shift where a US keyboard needs it; a character it has no key for is dropped.
A hardware keyboard's keys go through as they are pressed and released, all
but Back, the volume and the media keys.

The soft keyboard covers the bottom of the screen, which stays where it is.
The app tells the guest how much is covered, on its console, once the
compositor has printed `hyprix: card0 <monitor>` and two seconds have passed,
and each time the height has held for 150 ms and changed:

```sh
hyprctl keyword monitor Virtual-1,addreserved,0,<H>,0,0 >/dev/null 2>&1
```

`H` is the keyboard's and the row's height in the compositor's logical
pixels, the phone's divided by the monitor's scale, rounded up; the scale is
the mode's width over the logical width in `hyprix: 1 monitor [card0
Virtual-1 1080x2400 540x1200]`, and 1 without it. Closing the keyboard sends
0.
