#!/bin/sh
# Stub for steamos-select-branch — Steam calls this with -c to query the
# current update branch. Return "stable" to satisfy the check.
case "$1" in
    -c) echo "stable" ;;
    *)  echo "stable" ;;
esac
