#!/bin/sh
set -e

NAME="${1:-world}"

echo "Hello, ${NAME}! (from Docker action)"
echo "ACTION_ENV_TEST=${ACTION_ENV_TEST}"
echo "INPUT_NAME=${INPUT_NAME}"

# Test workflow commands work from inside a docker action
echo "::set-output name=greeting::Hello ${NAME}"
echo "::notice::Docker action completed for ${NAME}"
