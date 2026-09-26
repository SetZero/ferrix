# Boot Ferrix from the phone

An Android app with one button that reboots the Pixel 7 into Ferrix, and the
helper on the PC that does the booting.

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
