#!/bin/sh
# The monitor, in the Terminal window RadBeeper Monitor.app asked for. q quits.
#
# THE SERVICE FIRST, AS THE WINDOW'S APP DOES, and for its reason: a Mac has no boot service,
# and a monitor that took the port itself would stop the logging when its window was closed
# and would never ask the counter its temperature, which only the service does. So if
# nothing is serving the log directory the service is started and left running, and the
# monitor attaches to it. See RadBeeper.sh, which also says why it looks again only when a
# port appears or half a minute has gone by.
here=$(cd "$(dirname "$0")" && pwd)
logs="$HOME/.local/share/radbeeper"
mkdir -p "$logs"
serving() { "$here/radbeeper-cli" --logs "$logs" now >/dev/null 2>&1; }
ports() { ls /dev/cu.usbserial* 2>/dev/null | tr '\n' ' '; }

tried=none
since=0
said=
until serving; do
    have=$(ports)
    if [ -n "$have" ] && { [ "$have" != "$tried" ] || [ "$since" -ge 30 ]; }; then
        tried=$have
        since=0
        nohup "$here/radbeeper-cli" --logs "$logs" service >> "$logs/service.log" 2>&1 &
        sleep 3
        continue
    fi
    if [ -z "$said" ]; then
        said=yes
        echo "RadBeeper: no counter yet."
        echo
        echo "  Plug the GMC-320 in with its own USB cable and switch it on."
        echo "  A charge-only cable shows nothing."
        echo
        echo "  This window starts the monitor by itself when the counter answers."
        echo "  Close the window, or press Ctrl-C, to stop waiting."
    fi
    sleep 2
    since=$((since + 2))
done
# Room for the three papers: 132 columns by 44 rows.
printf '\033[8;44;132t'
exec "$here/radbeeper-cli" --logs "$logs" watch
