# `cargo xtask test-steam-store` (docs/STEAM.md): started by hyprix's
# exec-once beside desktop.sh, as root. It lists hyprix's windows each time
# they change, as a numbered snapshot the gate reads the last of: one line a
# window, its place, size, whether it floats and its title, then an end
# line. And it says what the client's connection log answers to each logon,
# the answer alone, without the account the line names.
export PATH=/bin:/data/usr/bin
log=/data/steam/logs/connection_log.txt
n=0
seen=
said=0
while :; do
    now=$(/bin/hyprctl clients 2>/dev/null | awk '
        /^\tat: / { at = $2 }
        /^\tsize: / { size = $2 }
        /^\tfloating: / { floating = $2 }
        /^\ttitle: / { sub(/^\ttitle: /, ""); print at " " size " " floating " " $0 }')
    if [ "$now" != "$seen" ]; then
        n=$((n + 1))
        echo "$now" | while IFS= read -r window; do
            [ -n "$window" ] && echo "steam-store: window $n: $window"
        done
        echo "steam-store: windows $n: end"
        seen=$now
    fi
    if [ -f $log ]; then
        answers=$(sed -n "s/.*RecvMsgClientLogOnResponse() : \[[^]]*\] '\([^']*\)'.*/\1/p" $log)
        count=$(echo "$answers" | grep -c .)
        if [ "$count" -gt "$said" ]; then
            echo "$answers" | tail -n $((count - said)) | sed 's/^/steam-store: logon: /'
            said=$count
        fi
    fi
    sleep 2
done
