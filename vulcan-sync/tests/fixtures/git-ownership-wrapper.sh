#!/bin/sh
# Keep $0 unresolved: each test symlinks this fixture into its isolated directory.
test_root=$(dirname "$0")
export GIT_CONFIG_GLOBAL="$test_root/global.config"
export GIT_CONFIG_NOSYSTEM=1
if test "$1" != clone && test -e "$test_root/different-owner"; then
    export GIT_TEST_ASSUME_DIFFERENT_OWNER=1
fi
exec git "$@"
