# Ferrix screenshots, 2026-09-27

All the images come from one boot of Ferrix at main 967bf3f5. The guest is x86_64
under QEMU with KVM, 4 vCPUs and 4 GiB of RAM, on a virtio-gpu 3D card (virgl). The
screen is 1920x1080 and was captured over VNC. Nothing was edited after capture. The
background is `ember`, the picture xtask draws when no wallpaper is kept or named. It is
not one of the kept wallpapers.

The load btop shows is real: Chrome loaded and scrolled a page every ten seconds or so
(rust-lang.org, Wikipedia, the Rust book, GitHub, docs.rs, the Rust blog) for about two
minutes before each btop capture. The git prompt is a small repository made in the
guest, `/src/hello`, with one commit and a clean tree.

## Captions

- **desktop-hero.png**: The hyprix Wayland compositor on Ferrix (x86_64, KVM) with three
  tiled windows: btop after two minutes of Chrome loading pages, zinc with its oh-my-zsh
  prompt in a git repository after `uname -a`, `git log` and
  `svc status hyprix.service`, and Chrome 154 showing rust-lang.org over the guest's
  network.
- **desktop-chrome.png**: Chrome 154 (Chrome for Testing) on Ferrix's hyprix compositor,
  showing the Wikipedia article on Rust. Chrome runs on ferrousli, Ferrix's own libc, and
  renders in software. x86_64, KVM.
- **terminal-omz.png**: zinc, Ferrix's zsh-compatible shell, running oh-my-zsh's
  agnoster prompt in hyprix's terminal, in a clean git repository. The output shows
  `uname`, the ring-3 driver processes (devmgr, input, blk, net, gpu), the services `svc`
  runs, a tmpfs root beside a btrfs volume, `git status`, and `curl` fetching
  rust-lang.org over HTTPS. x86_64, KVM.
- **btop.png**: btop, built against ferrousli, monitoring a live Ferrix system after two
  minutes of Chrome loading pages: the CPU and eth0 history, Chrome's processes, the
  compositor, the user-space drivers, memory and disks. x86_64, KVM.

## Commands

On the host (`<empty>` is an empty directory, so no kept wallpaper is found and `ember`
shows):

```
git -C /home/sebastian/Documents/projects/os/ferrix worktree add -b e4-shots .claude/worktrees/e4-shots main
FERRIX_WALLPAPERS=<empty> CARGO_TARGET_DIR=<target> \
  cargo xtask run-compositor --arch x86_64 --chrome --release --tmpfs-root --accel kvm \
    --vnc 127.0.0.1:74 --ssh 2374 --layout us
```

`--layout us` makes the guest read US keys, so the VNC driver's keysyms type correctly.

The git repository, over ssh (`ssh -i ~/.local/share/ferrix/ssh/id_ed25519 -p 2374
root@127.0.0.1`):

```
mkdir -p /src/hello/src && cd /src/hello
# a Cargo.toml and a src/main.rs that prints "Hello from Ferrix"
git init -q -b main && git add . && git commit -q -m "Say hello"
```

Driving the desktop (the steps given to `vncdrive.py 127.0.0.1 5974`, a copy with
`super` and a few more keysyms added). btop is never typed into or clicked: windows are
moved with `hyprctl` over ssh, which acts on the focused window.

- A second terminal: `key super Return ;`. In it,
  `hyprctl dispatch resizeactive 80 110` (btop needs 80x24 cells and a half-screen
  tile gives 77x21), then `clear; btop`
- Load, repeated per page: `click 1500 84 1 key ctrl a ; type <url> key Return ;
  sleep 7 wheel down 10 1470 600 sleep 2 wheel down 10 1470 600 sleep 2`
- In the other zinc: `cd /src/hello`, `uname -a`, `git --no-pager log --oneline`,
  `svc status hyprix.service`. Plain `git log` opens a pager, and a key meant for the
  shell can save a file into the repository and make the prompt dirty.
- The hero: a second round of loads, ending on `https://www.rust-lang.org`
- btop alone, over ssh: `hyprctl dispatch focuswindow pid:<btop's term>`,
  `hyprctl dispatch movetoworkspacesilent 4`, two minutes of loads in Chrome, then
  `hyprctl dispatch workspace 4`
- Chrome alone, over ssh: `hyprctl dispatch workspace 1`,
  `hyprctl dispatch focuswindow pid:<chrome>`, `hyprctl dispatch movetoworkspace 2`, then
  the address bar set to `https://en.wikipedia.org/wiki/Rust_(programming_language)`
- The terminal shot, over ssh: `hyprctl dispatch workspace 1` (zinc is alone there now),
  then `clear`, `uname -srm`, `ps -o pid,comm | head -12`, `svc list | grep running`,
  `df -h / /data`, `git status --short --branch`,
  `curl -sI https://rust-lang.org/ | head -3`
- Every capture: `move 1919 1079 shot <file>.png`
