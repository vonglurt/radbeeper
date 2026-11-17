#!/bin/sh
# What a double click on RadBeeper Monitor.app runs: a Terminal window with the monitor in it.
#
# AN APP CANNOT BE A TERMINAL PROGRAM, SO IT ASKS TERMINAL FOR ONE. The monitor draws in a
# terminal and needs one to draw in; a double click gives a program no terminal at all. So
# this hands Terminal the few lines beside it, watch.command, which Terminal opens a window
# for and runs.
here=$(cd "$(dirname "$0")" && pwd)
exec open -a Terminal "$here/watch.command"
