# The changes tools/busybox/build.sh makes to Alpine's busyboxconfig, each with
# its reason. Alpine builds that config into both its shared and its static
# busybox, so what is left unchanged here is exactly the applet set of the musl
# busybox Ferrix already runs.

set_config() { # file NAME value ("y", "n", or a quoted string)
    local file=$1 name=$2 value=$3
    sed -i -E "/^(# )?CONFIG_$name( is not set|=.*)$/d" "$file"
    if [ "$value" = n ]; then
        echo "# CONFIG_$name is not set" >> "$file"
    else
        echo "CONFIG_$name=$value" >> "$file"
    fi
}

apply_config_changes() { # file
    local c=$1
    # A static program: ferrousli has no dynamic loader yet, and Ferrix's
    # test-shell embeds one static binary.
    set_config "$c" STATIC y
    # Linked at a fixed address, as ferrousli's own programs are. Alpine's
    # config asks for a PIE, which a static ferrousli program cannot be yet.
    set_config "$c" PIE n
}
