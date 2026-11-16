#!/bin/sh
# What a double click on RadBeeper.app runs.
#
# A MAC HAS NO BOOT SERVICE, AND THE WINDOW READS ONE. On Copal the counter is
# read from boot by a service and the window attaches to it. Here nothing is
# reading the counter until somebody asks, so this asks: if no radbeeper is
# serving the log directory it starts the service, and LEAVES IT RUNNING when
# the window is closed, so the record has no hole in it for the evening the
# window was shut. `radbeeper now` is the question -- it exits 1 when nothing
# is serving -- and it asks the service, never the counter.
#
# With no counter plugged in the service says so in its log and stops, and the
# window opens and waits; plug one in and open the app again.
here=$(cd "$(dirname "$0")" && pwd)
logs="$HOME/.local/share/radbeeper"
mkdir -p "$logs"
if ! "$here/radbeeper-cli" --logs "$logs" now >/dev/null 2>&1; then
    nohup "$here/radbeeper-cli" --logs "$logs" service >> "$logs/service.log" 2>&1 &
    sleep 2
fi
exec "$here/radbeeper-gui" --logs "$logs" "$@"
