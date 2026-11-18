#!/bin/sh
# Normal directory enumeration of $1 in the format of `synoreclaim debug
# tree`: path, find type letter, inode, size ("-" for directories), sorted
# bytewise. -xdev lists nested subvolume roots without descending.
set -eu
cd "$1"
find . -xdev -mindepth 1 \
    \( -type d -printf '%P\td\t%i\t-\n' \) -o -printf '%P\t%y\t%i\t%s\n' |
    LC_ALL=C sort
