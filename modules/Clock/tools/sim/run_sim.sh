#!/bin/bash
# Build the clock firmware (release, with symbols) and run it in simavr.
#
#   tools/sim/run_sim.sh <seconds> [scenario.txt]
#   tools/sim/analyze.py portd.log      # edge timing / jitter per channel
#
# Needs simavr's library and headers: `sudo apt install libsimavr-dev`, or point
# SIMAVR_ROOT at a directory containing usr/include/simavr and usr/lib/... (for
# example extracted with `apt-get download libsimavr2 libsimavr-dev` + `dpkg-deb -x`).
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
FW=$HERE/../../Firmware
ROOT=${SIMAVR_ROOT:-/}
LIBDIR=$(dirname "$(find "$ROOT/usr/lib" -name 'libsimavr.so.2' 2>/dev/null | head -1)")
OUT=${OUT_DIR:-$HERE/out}
mkdir -p "$OUT"
if [ ! -x "$OUT/harness" ] || [ "$HERE/harness.c" -nt "$OUT/harness" ]; then
    gcc -O2 -o "$OUT/harness" "$HERE/harness.c" -I "$ROOT/usr/include/simavr" \
        -I "$ROOT/usr/include/simavr/avr" -L "$LIBDIR" -l:libsimavr.so.2 -l:libelf.so.1 \
        -Wl,-rpath,"$LIBDIR"
fi
(cd "$FW" && CARGO_PROFILE_RELEASE_STRIP=none CARGO_TARGET_DIR="$OUT/target" \
    cargo build --release 2>&1 | grep -E '^error' -A5 || true)
ELF=$OUT/target/avr-atmega328p/release/fm-clock.elf
avr-size "$ELF" | tail -1
read -r ADDR LEN <<<"$(avr-nm -S "$ELF" | awk '/ __vector_11$/{print $1, $2}')"
SCRIPT=${2:+$(realpath "$2")}
cd "$OUT" && ./harness "$ELF" "$1" $((16#$ADDR)) $((16#$ADDR + 16#$LEN)) $SCRIPT 2>&1 | grep -v '^Loaded'
echo "output log: $OUT/portd.log"
