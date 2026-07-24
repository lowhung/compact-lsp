#!/bin/sh

# A direct formatter writes formatted source to stdout. Echoing the temporary
# input proves request wiring without making the fixture depend on a toolchain.
if [ "$#" -ne 1 ]; then
  printf '%s\n' 'expected one Compact source path' >&2
  exit 2
fi

sed -n '1,$p' "$1"
