# Stands in for the Steam Runtime's logger-0.bash (docs/STEAM.md), which the
# client's scripts source to send their output through srt-logger. `cargo
# xtask run-steam` carries it under /steam/scout, which client.sh names in
# STEAM_RUNTIME_SCOUT, and it logs nothing: the output stays on stderr.
# Valve's needs what steam-proc-gaps is fixing; drop this when it lands.
return 0
