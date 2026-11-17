#!/bin/sh
# The monitor, in the Terminal window RadBeeper Monitor.app asked for. q quits.
#
# THE SERVICE FIRST, AS THE WINDOW'S APP DOES, and for its reason: a Mac has no boot service,
# and a monitor that took the port itself would stop the logging when its window was closed
# and would never ask the counter its temperature, which only the service does. So if
# nothing is serving the log directory the service is started and left running, and the
# monitor attaches to it. See RadBeeper.sh.
here=$(cd "$(dirname "$0")" && pwd)
logs="$HOME/.local/share/radbeeper"
mkdir -p "$logs"
if ! "$here/radbeeper-cli" --logs "$logs" now >/dev/null 2>&1; then
    nohup "$here/radbeeper-cli" --logs "$logs" service >> "$logs/service.log" 2>&1 &
    sleep 2
fi
# Room for the three papers: 132 columns by 44 rows.
printf '\033[8;44;132t'
exec "$here/radbeeper-cli" --logs "$logs" watch
