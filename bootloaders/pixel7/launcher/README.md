# Boot Ferrix from the phone

An Android app with two ways to run Ferrix on the Pixel 7, and the helper on
the PC that the first one needs:

* **Boot Ferrix** reboots the phone into Ferrix, through the PC.
* **Run in a VM** runs Ferrix beside Android, as a guest of the phone's own
  KVM through Android's crosvm. No PC and no reboot are needed, and it takes
  about 6 seconds to `FERRIX-BOOT-OK`.

The phone cannot start Ferrix by itself. Its kernel has no `kexec`, and
nothing may be flashed (`../HANDOVER.md`, "Never write anything that survives
a reset"). What starts Ferrix is `fastboot boot` from a PC, so the button asks
the PC. The phone has to be plugged into it, and nothing on the phone is
changed: Ferrix runs from RAM, and its watchdog brings Android back about 75
seconds later.

```
phone: Boot Ferrix ──HTTP to 127.0.0.1:47707──▶ adb reverse ──USB──▶ helper.py on the PC
helper: adb reboot bootloader → fastboot stage vendor_boot.img → fastboot boot boot.img
        → wait for Android → save the ramoops record → report FERRIX-BOOT-OK or the panic
```

## The helper

```sh
python3 bootloaders/pixel7/launcher/helper.py              # the newest $P/*/boot.img
python3 bootloaders/pixel7/launcher/helper.py --image PATH # a given one
```

`P` is `~/.local/share/ferrix/pixel7`, where `vendor_boot.img` and the run
directories are. The helper listens on 127.0.0.1 only and keeps the phone's
`adb reverse` pointing at it; a reboot drops it, and it is put back. Each run's
record lands in `$P/launcher-<time>/run.log`. The endpoints are `GET /status`
and `POST /boot`. Build an image first with `$P/build-run.sh`, as the
handover's "Running the phone with nobody there" says.

## The app

Kotlin and Jetpack Compose, Material 3 with the wallpaper's dynamic colours.
The Gradle project here builds offline from the cache PhoneLink's build left,
with the SDK in `~/Android/Sdk` and a JDK that has `javac`:

```sh
cd bootloaders/pixel7/launcher
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

## The VM

The app runs, through `su` (Magisk asks once):

```sh
cd /data/local/tmp/ferrix-vm && /apex/com.android.virt/bin/crosvm run \
    --disable-sandbox -m 4096 --cpus 8 --serial type=stdout,num=1 ferrix.Image
```

`ferrix.Image` is the raw loader `Image` from the helper's run directory, the
same loader and kernel `fastboot boot` gets, which the helper pushes whenever
the phone's copy differs. It needs to have been built with guest support
(`9df3769b` or later). The loader finds it is a guest, entered at EL1 with
crosvm's 16550 as `stdout-path`, and sends its log and the kernel's there.
crosvm's machine has 2 to 8 vCPUs, GICv3, PSCI through `hvc`, and RAM at
`0x8000_0000`, with no screen and no virtio devices yet. The guest powers off
at the end of its run, and crosvm exits.

Starting a VM opens it full screen: one bar, with the run's state and a menu
to pause or resume it, stop it, run it again, or go back, and the console
under it. The console follows the newest line while the reader is within ten
lines of the end. Pause and stop go to crosvm's control socket
(`crosvm suspend|resume|stop /data/local/tmp/ferrix-vm/crosvm.sock`). A
paused guest's counter keeps running, so pausing during the boot's
self-checks can fail a timing check, as it did once in stage 3 (FX-0302).
