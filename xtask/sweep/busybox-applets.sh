# Every busybox applet that can be run harmlessly, for `cargo xtask sweep`.
#
#   cargo xtask sweep --arch x86_64 --timeout 3600 \
#       --init '<busybox>/{arch}/bin/busybox.static' \
#       --script xtask/sweep/busybox-applets.sh
#
# Everything above the first `### NAME` line is put in front of every snippet.
# Each snippet runs as its own `sh -c`, so the kernel brackets it in the log and
# reports the calls it refused; each step inside one closes with `rc NAME
# STATUS`, which is what charges a refusal to the program that asked. See
# `xtask/src/sweep.rs`.
#
# Every step runs under `timeout -s KILL`, so an applet that waits forever
# costs its timeout rather than the sweep. Steps that need the console for
# stdin and stdout (`rt`) come last, because a program killed while it holds
# the terminal in raw mode leaves it that way.
#
# Not run, each for its reason:
#   reboot, poweroff, halt              end the machine
#   init, linuxrc                       would signal or try to become PID 1
#   killall5                            signals every process, the sweep included
#   pivot_root                          would replace / for the rest of the sweep
#   watchdog                            arms a watchdog that resets the machine
#   resume                              resumes from a hibernation image
#   mkfs.vfat, mkdosfs, mke2fs          format devices
#   blkdiscard, freeramdisk             destroy a block device's contents
#   raidautorun                         starts RAID arrays
#   fsfreeze                            freezing a mounted fs can stall the sweep
#   nandwrite, i2cset, i2ctransfer      write to hardware
#   chvt, openvt, deallocvt, setconsole change the console's virtual terminal
#   loadkmap, loadfont, setfont         replace the console's keymap or font
#   setkeycodes, setlogcons, showkey    change keyboard or console-log state
#   reset, resize                       rewrite the console's terminal settings
#   vlock                               locks the console against input
#   fbsplash                            draws on the framebuffer
#   stty (other than -a)                changes the console's settings
#   swapon, swapoff on a device         only a file under /tmp is used
#   hwclock -w/-s, date -s              set clocks (only reads are run)

S=/tmp/sw
show() {
    i=0
    while [ "$i" -lt 6 ] && IFS= read -r l; do
        echo "| $l"
        i=$((i + 1))
    done < "$S/out"
}
# r NAME PROG ARGS...: stdin from /dev/null, output to a file, 20 s.
r() { n=$1; shift; timeout -s KILL 20 "$@" < /dev/null > "$S/out" 2>&1; s=$?; show; echo "rc $n $s"; }
# q NAME SECS PROG ARGS...: as r, with its own timeout, for daemons.
q() { n=$1; t=$2; shift 2; timeout -s KILL "$t" "$@" < /dev/null > "$S/out" 2>&1; s=$?; show; echo "rc $n $s"; }
# rs NAME SCRIPT: a pipeline, in a child shell, as r.
rs() { n=$1; timeout -s KILL 20 sh -c "$2" < /dev/null > "$S/out" 2>&1; s=$?; show; echo "rc $n $s"; }
# rt NAME SECS PROG ARGS...: on the console, killed after SECS.
rt() { n=$1; t=$2; shift 2; timeout -s KILL "$t" "$@"; s=$?; echo; echo "rc $n $s"; }

### 000-shell-alone
echo "rc sh-startup 0"
### 001-setup
mkdir -p /tmp/sw/d /tmp/sw/www /tmp/sw/rp /tmp/sw/mnt /tmp/sw/cron /tmp/sw/nukeme/x
echo "rc mkdir $?"
printf 'banana\napple\ncherry\napple\n' > /tmp/sw/fruit
printf 'banana\napricot\ncherry\n' > /tmp/sw/fruit.b
printf 'a\tb\tc\n' > /tmp/sw/tabs
printf 'dos\r\nline\r\n' > /tmp/sw/dos
printf 'not a module\n' > /tmp/sw/junk.ko
printf 'start 10.0.0.10\nend 10.0.0.20\ninterface lo\nlease_file /tmp/sw/leases\n' > /tmp/sw/udhcpd.conf
: > /tmp/sw/leases
echo hello > /tmp/sw/www/index.html
: > "$S/out"
echo "rc setup-files 0"
### 002-timeout-alone
r timeout-true true
timeout -s TERM 1 sleep 5
echo "rc timeout-expires $?"
r ls-dev ls -la /dev
### files-1
r ls ls -la /tmp/sw
r cp cp /tmp/sw/fruit /tmp/sw/fruit2
r mv mv /tmp/sw/fruit2 /tmp/sw/fruit3
r ln ln -s fruit /tmp/sw/fruit.lnk
r link link /tmp/sw/fruit /tmp/sw/fruit.hard
r unlink unlink /tmp/sw/fruit.hard
r readlink readlink /tmp/sw/fruit.lnk
r realpath realpath /tmp/sw/fruit.lnk
r rm rm /tmp/sw/fruit3
### files-2
r mkdir mkdir /tmp/sw/e
r rmdir rmdir /tmp/sw/e
r touch touch -d '2020-01-01 00:00:00' /tmp/sw/fruit.t
r stat stat /tmp/sw/fruit
r stat-f stat -f /tmp
r chmod chmod 600 /tmp/sw/fruit.t
r chown chown 0:0 /tmp/sw/fruit.t
r chgrp chgrp root /tmp/sw/fruit.t
r install install -m 644 /tmp/sw/fruit /tmp/sw/inst
### files-3
r mktemp mktemp -p /tmp/sw
r mkfifo mkfifo /tmp/sw/fifo
r mknod mknod /tmp/sw/null c 1 3
rs mknod-write 'echo x > /tmp/sw/null'
r truncate truncate -s 4096 /tmp/sw/trunc
r fallocate fallocate -l 8192 /tmp/sw/falloc
r fsync fsync /tmp/sw/trunc
r sync sync
r readahead readahead /tmp/sw/fruit
r shred shred -n 1 -u /tmp/sw/inst
### files-4
r du du -s /tmp/sw
r df df
r df-h df -h /tmp
r find find /tmp/sw -type f -name 'f*'
r tree tree /tmp/sw
r basename basename /a/b.txt .txt
r dirname dirname /a/b.txt
r pwd pwd
r which which ls
r mountpoint mountpoint /proc
### files-attrs
r lsattr lsattr /tmp/sw/fruit
r chattr chattr -d /tmp/sw/fruit
r fatattr fatattr /tmp/sw/fruit
r getfattr getfattr -d /tmp/sw/fruit
r nuke nuke /tmp/sw/nukeme
### text-1
r cat cat /tmp/sw/fruit
r sort sort /tmp/sw/fruit
r uniq uniq -c /tmp/sw/fruit
r wc wc /tmp/sw/fruit
r head head -n 2 /tmp/sw/fruit
r tail tail -n 2 /tmp/sw/fruit
r tac tac /tmp/sw/fruit
r rev rev /tmp/sw/fruit
### text-2
r grep grep -n apple /tmp/sw/fruit
r egrep egrep 'a|c' /tmp/sw/fruit
r fgrep fgrep apple /tmp/sw/fruit
r sed sed 's/a/A/g' /tmp/sw/fruit
r awk awk '{ n += length($0) } END { print n }' /tmp/sw/fruit
r cut cut -c1-3 /tmp/sw/fruit
r paste paste /tmp/sw/fruit /tmp/sw/fruit
rs tr 'echo abc | tr a-z A-Z'
### text-3
r nl nl /tmp/sw/fruit
r fold fold -w 3 /tmp/sw/fruit
r expand expand /tmp/sw/tabs
r unexpand unexpand -a /tmp/sw/tabs
r comm comm /tmp/sw/fruit /tmp/sw/fruit
r cmp cmp /tmp/sw/fruit /tmp/sw/fruit
rs diff 'diff -u /tmp/sw/fruit /tmp/sw/fruit.b > /tmp/sw/fruit.diff; test $? -le 1'
rs patch 'cp /tmp/sw/fruit /tmp/sw/fruit.p && patch /tmp/sw/fruit.p < /tmp/sw/fruit.diff'
r split split -l 2 /tmp/sw/fruit /tmp/sw/split.
r shuf shuf /tmp/sw/fruit
### text-4
r strings strings /tmp/sw/fruit
r dos2unix dos2unix /tmp/sw/dos
r unix2dos unix2dos /tmp/sw/dos
r seq seq 1 5
rs yes 'yes | head -n 3'
rs xargs 'echo a b c | xargs -n1 echo'
rs tee 'echo hi | tee /tmp/sw/tee'
r printf printf '%s-%d\n' a 1
r echo echo hi
r env env
r printenv printenv PATH
### text-5
r expr expr 3 + 4
r test test -f /tmp/sw/fruit
r [ [ -d /tmp ]
r [[ [[ -d /tmp ]]
r true true
r false false
r getopt getopt ab: -a -b x
r factor factor 360
rs bc 'echo "2^10" | bc'
rs dc 'echo "2 3 + p" | dc'
r cal cal 1 2026
r ascii ascii
rs ed 'printf "1p\nq\n" | ed /tmp/sw/fruit'
### bytes
r hexdump hexdump -C /tmp/sw/fruit
r hd hd /tmp/sw/fruit
r od od -c /tmp/sw/fruit
r xxd xxd /tmp/sw/fruit
r dd dd if=/dev/zero of=/tmp/sw/zero bs=1k count=64
r dd-urandom dd if=/dev/urandom of=/tmp/sw/rand bs=512 count=1
r base64 base64 /tmp/sw/fruit
rs uuencode 'uuencode /tmp/sw/fruit /tmp/sw/uu.out > /tmp/sw/uu'
r uudecode uudecode -o /tmp/sw/uu2 /tmp/sw/uu
### sums
r md5sum md5sum /tmp/sw/fruit
r sha1sum sha1sum /tmp/sw/fruit
r sha256sum sha256sum /tmp/sw/fruit
r sha512sum sha512sum /tmp/sw/fruit
r sha3sum sha3sum /tmp/sw/fruit
r cksum cksum /tmp/sw/fruit
r sum sum /tmp/sw/fruit
r crc32 crc32 /tmp/sw/fruit
### archive-tar
r tar-c tar -cf /tmp/sw/a.tar -C /tmp/sw fruit
r tar-t tar -tvf /tmp/sw/a.tar
r tar-x tar -xf /tmp/sw/a.tar -C /tmp/sw/d
r tar-z tar -czf /tmp/sw/a.tgz -C /tmp/sw fruit
r tar-zx tar -xzf /tmp/sw/a.tgz -C /tmp/sw/d
rs cpio-o 'cd /tmp/sw && echo fruit | cpio -o -H newc > /tmp/sw/a.cpio'
rs cpio-t 'cpio -it < /tmp/sw/a.cpio'
r ar ar -t /tmp/sw/a.tar
### archive-gzip
rs gzip 'gzip -c /tmp/sw/fruit > /tmp/sw/f.gz'
r gunzip gunzip -c /tmp/sw/f.gz
r zcat zcat /tmp/sw/f.gz
r uncompress uncompress -c /tmp/sw/f.gz
rs bzip2 'bzip2 -c /tmp/sw/fruit > /tmp/sw/f.bz2'
r bunzip2 bunzip2 -c /tmp/sw/f.bz2
r bzcat bzcat /tmp/sw/f.bz2
rs lzop 'lzop -c /tmp/sw/fruit > /tmp/sw/f.lzo'
r unlzop unlzop -c /tmp/sw/f.lzo
r lzopcat lzopcat /tmp/sw/f.lzo
### archive-xz
rs xz-data 'echo /Td6WFoAAATm1rRGBMAQDCEBFgAAAAAAAAAAAHuwVCgBAAtoZWxsbyBzd2VlcAoAhea/N2OQfogAASwMrpIBEB+2830BAAAAAARZWg== | base64 -d > /tmp/sw/f.xz'
r unxz unxz -c /tmp/sw/f.xz
r xzcat xzcat /tmp/sw/f.xz
r xz xz -dc /tmp/sw/f.xz
rs lzma-data 'echo XQAAgAD//////////wA0GUnujekWcvxYmUYwYQf//+B/gAA= | base64 -d > /tmp/sw/f.lzma'
r unlzma unlzma -c /tmp/sw/f.lzma
r lzcat lzcat /tmp/sw/f.lzma
r lzma lzma -dc /tmp/sw/f.lzma
rs zip-data 'echo UEsDBBQAAAAIAFVVLV2EJriIDgAAAAwAAAAJAAAAaGVsbG8udHh0y0jNyclXKC5PTS3gAgBQSwECFAMUAAAACABVVS1dhCa4iA4AAAAMAAAACQAAAAAAAAAAAAAAgAEAAAAAaGVsbG8udHh0UEsFBgAAAAABAAEANwAAADUAAAAAAA== | base64 -d > /tmp/sw/f.zip'
r unzip unzip -o /tmp/sw/f.zip -d /tmp/sw/d
### archive-packages
r rpm rpm -qa
r rpm2cpio rpm2cpio /tmp/sw/fruit
r dpkg dpkg -l
r dpkg-deb dpkg-deb -I /tmp/sw/fruit
### procs-1
r ps ps
r ps-o ps -o pid,ppid,stat,comm
r top-batch top -b -n 1
r pstree pstree
r pgrep pgrep sh
r pidof pidof sh
r pkill pkill -0 nonexistent_xyz
r killall killall -0 nonexistent_xyz
### procs-2
rs kill 'sleep 5 & kill $!; wait $!'
r kill-l kill -l
r free free
r uptime uptime
r w w
r who who
r last last
rs pmap 'pmap $$'
rs pwdx 'pwdx $$'
### procs-3
r lsof lsof
r fuser fuser -m /tmp
r iostat iostat
r mpstat mpstat
q nmeter 3 nmeter -d 1000 '%t %c'
q watch-batch 3 watch -n 1 -t true
r time time true
### sched
r nice nice -n 5 true
rs renice 'renice -n 1 -p $$'
rs ionice 'ionice -p $$'
r ionice-idle ionice -c 3 true
rs taskset 'taskset -p $$'
r taskset-run taskset 1 true
r nproc nproc
rs ulimit 'ulimit -a'
r usleep usleep 1000
r sleep sleep 0.1
### session
rs nohup 'cd /tmp/sw && nohup true'
r setsid setsid true
r flock flock /tmp/sw/lock true
r start-stop-daemon start-stop-daemon -S -t -x /bin/true
r run-parts run-parts --test /tmp/sw/rp
rs pipe_progress 'echo hi | pipe_progress'
rs ts 'echo hi | ts'
r svc svc -u /tmp/sw/svc
r svok svok /tmp/sw/svc
r mim mim
r bbconfig bbconfig
### namespaces
r linux32 linux32 uname -m
r linux64 linux64 uname -m
r setpriv setpriv -d
rs nsenter 'nsenter -t $$ -m true'
r unshare unshare -m true
r chroot chroot / /bin/true
r ash ash -c true
r sh sh -c true
r static-sh static-sh -c true
### daemons
q crond 3 crond -f -d 8 -c /tmp/sw/cron
r crontab crontab -l -c /tmp/sw/cron
q inotifyd 3 inotifyd true /tmp/sw:c
q uevent 3 uevent true
q acpid 3 acpid -f -d
q syslogd 3 syslogd -n -O /tmp/sw/messages
q syslogd-shm 3 syslogd -n -C16
q klogd 3 klogd -n
### logs
r logger logger sweep
r logread logread
r dmesg dmesg
r ipcs ipcs
r ipcrm ipcrm -m 12345
### sysinfo-1
r uname uname -a
r arch arch
r hostid hostid
r hostname hostname
rs hostname-set 'hostname "$(hostname)"'
r dnsdomainname dnsdomainname
r date date
r date-epoch date -u +%s
### sysinfo-2
r hwclock hwclock -r
r adjtimex adjtimex
r sysctl sysctl -a
r sysctl-one sysctl kernel.hostname
r id id
r whoami whoami
r groups groups
r logname logname
r rdev rdev
### sysinfo-3
r mount mount
r lsmod lsmod
r modinfo modinfo dummy
r modprobe modprobe -n dummy
r depmod depmod -n
r insmod insmod /tmp/sw/junk.ko
r rmmod rmmod dummy
r lsusb lsusb
r lsscsi lsscsi
### devices-1
r fbset fbset
r setserial setserial -g /dev/console
r blkid blkid
r findfs findfs LABEL=nothing
r blockdev blockdev --getsize64 /dev/null
r fdisk-l fdisk -l
r fstrim fstrim /tmp
r losetup losetup -a
r partprobe partprobe /dev/null
### devices-2
r eject eject /dev/null
r mt mt -f /dev/null status
r fdflush fdflush /dev/null
r volname volname /dev/null
r nanddump nanddump /dev/mtd0
r ubirename ubirename /dev/ubi0 a b
r nbd-client nbd-client -d /dev/nbd0
r i2cdetect i2cdetect -l
r i2cget i2cget -y 0 0x50 0
r i2cdump i2cdump -y 0 0x50
r rfkill rfkill list
r devmem devmem 0x0 8
### swap-fsck
r fsck fsck -N /dev/null
r swapfile dd if=/dev/zero of=/tmp/sw/swap bs=1k count=256
r mkswap mkswap /tmp/sw/swap
r swapon swapon /tmp/sw/swap
r swapoff swapoff /tmp/sw/swap
### net-read-1
r ip-addr ip addr
r ip-link ip link
r ip-route ip route
r ip-neigh ip neigh
r ip-rule ip rule
r ipaddr ipaddr
r iplink iplink
r iproute iproute
r ipneigh ipneigh
r iprule iprule
### net-read-2
r iptunnel iptunnel show
r ifconfig ifconfig -a
r route route -n
r netstat netstat -an
r arp arp -a
r brctl brctl show
r tc tc qdisc show
r ipcalc ipcalc -n 192.168.1.1/24
r nameif nameif
r ifenslave ifenslave
### net-config
r vconfig vconfig rem eth9.9
r tunctl tunctl -t tap9
r ifup ifup -n -a
r ifdown ifdown -n -a
q slattach 3 slattach -p slip /dev/null
q zcip 5 zcip -f -q lo /bin/true
q udhcpc 5 udhcpc -f -q -n -t 1 -T 1 -i lo -s /bin/true
q udhcpc6 5 udhcpc6 -f -q -n -t 1 -T 1 -i lo -s /bin/true
q udhcpd 3 udhcpd -f /tmp/sw/udhcpd.conf
r dumpleases dumpleases -f /tmp/sw/leases
### net-probe
q ping 5 ping -c 1 -W 1 127.0.0.1
q ping6 5 ping6 -c 1 -W 1 ::1
q arping 5 arping -c 1 -w 1 -I lo 127.0.0.1
q traceroute 5 traceroute -m 1 -w 1 -q 1 127.0.0.1
q traceroute6 5 traceroute6 -m 1 -w 1 -q 1 ::1
q ether-wake 5 ether-wake -i lo 00:11:22:33:44:55
q pscan 5 pscan -p 1 -P 3 -t 1 -T 1 127.0.0.1
q nslookup 5 nslookup localhost 127.0.0.1
### net-clients
q whois 5 whois -h 127.0.0.1 example
q telnet 3 telnet 127.0.0.1 23
q wget 5 wget -T 2 -O /tmp/sw/w http://127.0.0.1:8080/
q ftpget 5 ftpget 127.0.0.1 /tmp/sw/x x
q ftpput 5 ftpput 127.0.0.1 x /tmp/sw/fruit
q tftp 5 tftp -g -r x 127.0.0.1
q sendmail 5 sendmail -S 127.0.0.1 -f a@b c@d
q rdate 5 rdate -p 127.0.0.1
q ntpd 5 ntpd -n -q -p 127.0.0.1
r ssl_client ssl_client
q microcom 3 microcom /dev/null
### net-servers
rs nc-pair 'nc -l -p 12345 > /tmp/sw/nc.out & p=$!; sleep 1; echo hi | nc -w 2 127.0.0.1 12345; s=$?; kill $p; exit $s'
rs httpd-wget 'httpd -f -p 127.0.0.1:8080 -h /tmp/sw/www & p=$!; sleep 1; wget -T 3 -O - http://127.0.0.1:8080/; s=$?; kill $p; exit $s'
q telnetd 3 telnetd -F -p 2323 -l /bin/true
### users
rs backup 'cp /etc/passwd /tmp/sw/passwd.orig && cp /etc/group /tmp/sw/group.orig'
r cryptpw cryptpw -m sha512 secret
r mkpasswd mkpasswd -m md5 secret
r addgroup addgroup sweepgrp
r adduser adduser -D -H -h /tmp/sw -s /bin/sh -G sweepgrp sweepuser
rs chpasswd 'echo sweepuser:secret | chpasswd'
r passwd-l passwd -l sweepuser
r passwd-u passwd -u sweepuser
r su-user su -s /bin/sh -c id sweepuser
r su-root su -c true root
r deluser deluser sweepuser
r delgroup delgroup sweepgrp
r add-shell add-shell /bin/sweepsh
r remove-shell remove-shell /bin/sweepsh
r nologin nologin
q getty 3 getty -n -l /bin/true 38400 /dev/null
rs restore 'cp /tmp/sw/passwd.orig /etc/passwd && cp /tmp/sw/group.orig /etc/group'
### late-mounts
r mount-tmpfs mount -t tmpfs none /tmp/sw/mnt
r mount-list mount
r umount umount /tmp/sw/mnt
r switch_root switch_root /tmp/sw/d /bin/true
r run-init run-init -n /tmp/sw/d /bin/true
r mdev mdev -s
### tty-1
rt stty-a 5 stty -a
rt tty 5 tty
rt ttysize 5 ttysize
rt mesg 5 mesg
rt clear 5 clear
rt kbd_mode 5 kbd_mode
rs dumpkmap 'dumpkmap > /tmp/sw/kmap'
rt beep 5 beep -f 1000 -l 1
rt cttyhack 5 cttyhack true
### tty-less
rt less 3 less /tmp/sw/fruit
rt more 3 more /tmp/sw/fruit
### tty-vi
rt vi 3 vi /tmp/sw/vi.txt
### tty-top
rt top 3 top
### tty-watch
rt watch 3 watch -n 1 -t true
### tty-script
rt script 5 script -q -c true /tmp/sw/typescript
### tty-login
rt login 3 login
rt login-f 3 login -f root
### tty-su
rt su-shell 3 su
rt sulogin 3 sulogin
### zzz-done
echo "rc sweep-finished 0"
