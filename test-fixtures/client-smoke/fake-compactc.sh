#!/bin/sh

# Keep editor smoke tests hermetic while still exercising compiler process startup.
if [ "$1" = "--version" ]; then
  printf '%s\n' 'compactc 0.33.0 (language 0.25)'
fi

exit 0
