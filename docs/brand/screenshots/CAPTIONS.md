# Ferrix screenshots, 2026-09-27

All the images come from one boot of Ferrix at main 2825c3e2. The guest is x86_64
under QEMU with KVM, 4 vCPUs and 4 GiB of RAM, on a virtio-gpu 3D card (virgl). The
screen is 1920x1080 and was captured over VNC. Nothing was edited after capture. The
background is an abstract gradient made for these shots (a generated PNG, converted with
`cargo xtask wallpapers`). It is not one of the kept wallpapers.

## Captions

- **desktop-hero.png**: The hyprix Wayland compositor on Ferrix (x86_64, KVM) with three
  tiled windows: btop, zinc with its oh-my-zsh prompt after `uname -a`,
  `cat /proc/version` and `svc status hyprix.service`, and Chrome 154 showing
  rust-lang.org over the guest's network.
- **desktop-chrome.png**: Chrome 154 (Chrome for Testing) on Ferrix's hyprix compositor,
  showing the Wikipedia article on Rust. Chrome runs on ferrousli, Ferrix's own libc, and
  renders in software. x86_64, KVM.
- **terminal-omz.png**: zinc, Ferrix's zsh-compatible shell, running oh-my-zsh's prompt
  in hyprix's terminal. The output shows `uname`, the ring-3 driver processes (devmgr,
  input, blk, net, gpu), the services `svc` runs, a tmpfs root beside a btrfs volume, and
  `curl` fetching rust-lang.org over HTTPS. x86_64, KVM.
- **btop.png**: btop, built against ferrousli, monitoring a live Ferrix system: Chrome's
  processes, the compositor, the user-space drivers, memory, disks and eth0. x86_64, KVM.

## Commands

On the host:

```
git -C /home/sebastian/Documents/projects/os/ferrix worktree add -b shots .claude/worktrees/shots main
# generated wallpapers/ferrix-ember.png (PIL gradient), then:
FERRIX_WALLPAPERS=<scratch>/walls CARGO_TARGET_DIR=<worktree>/target \
  cargo xtask wallpapers --from <scratch>/wallsrc
FERRIX_WALLPAPERS=<scratch>/walls CARGO_TARGET_DIR=<worktree>/target \
  cargo xtask run-compositor --arch x86_64 --chrome --release --tmpfs-root --accel kvm \
    --vnc 127.0.0.1:61 --ssh 2297 --layout us --wallpaper ember
```

`--layout us` makes the guest read US keys, so the VNC driver's keysyms type correctly.

Driving the desktop (the steps given to `vncdrive.py 127.0.0.1 5961`, a copy with
`super` and a few more keysyms added):

- Chrome's address bar: `click 1380 84 1 key ctrl a ; type https://www.rust-lang.org key Return ;`
- In zinc: `uname -a`, `cat /proc/version`, `svc status hyprix.service`
- A second terminal: `key super Return ;`. In it,
  `hyprctl dispatch resizeactive 80 110` (btop needs 80x24 cells and a half-screen
  tile gives 77x21), then `clear; btop`
- Chrome alone: `key super shift 2 ;` then `key super 2 ;`, then the address bar set to
  `https://en.wikipedia.org/wiki/Rust_(programming_language)`
- The terminal shot: `hyprctl dispatch movetoworkspace 3`, then
  `uname -srm`, `ps -o pid,comm | head -12`, `svc list | grep running`,
  `df -h / /data`, `curl -sI https://rust-lang.org/ | head -3`
- Every capture: `move 1919 1079 shot <file>.png`
