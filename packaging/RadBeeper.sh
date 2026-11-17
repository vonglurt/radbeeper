#!/bin/sh
# What a double click on RadBeeper.app runs.
#
# A MAC HAS NO BOOT SERVICE, AND THE WINDOW READS ONE. On Copal the counter is
# read from boot by a service and the window attaches to it. Here nothing is
# reading the counter until somebody asks, so this asks: while the window is
# open and nothing is serving the log directory, it starts the service, and
# LEAVES IT RUNNING when the window is closed, so the record has no hole in it
# for the evening the window was shut. `radbeeper now` is the question -- it
# exits 1 when nothing is serving -- and it asks the service, never the counter.
#
# IT KEEPS LOOKING, AND NOT TOO OFTEN. The service stops at once when it finds
# no counter, and the first version of this started it once and gave up: plug
# the counter in a minute later and the window said "no counter" for ever. So
# it is tried again whenever a serial port appears or goes, and otherwise every
# half minute while there is a port at all -- a counter plugged in but switched
# off has a port and no answer. Not every second: looking for a counter means
# opening every USB serial port and asking it, and a port that is somebody
# else's radio is restarted by being opened.
here=$(cd "$(dirname "$0")" && pwd)
logs="$HOME/.local/share/radbeeper"
mkdir -p "$logs"
serving() { "$here/radbeeper-cli" --logs "$logs" now >/dev/null 2>&1; }
ports() { ls /dev/cu.usbserial* 2>/dev/null | tr '\n' ' '; }

"$here/radbeeper-gui" --logs "$logs" "$@" &
gui=$!
(
    tried=none
    since=0
    while kill -0 "$gui" 2>/dev/null; do
        if ! serving; then
            have=$(ports)
            if [ -n "$have" ] && { [ "$have" != "$tried" ] || [ "$since" -ge 30 ]; }; then
                tried=$have
                since=0
                nohup "$here/radbeeper-cli" --logs "$logs" service >> "$logs/service.log" 2>&1 &
            fi
        fi
        sleep 2
        since=$((since + 2))
    done
) &
wait "$gui"
