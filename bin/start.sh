#!/bin/bash

set -ex

# run migrations for each necessary project
pushd migrations/spot
migrant setup
migrant list
migrant apply -a || true
popd

pushd migrations/paste
migrant setup
migrant list
migrant apply -a || true
popd

exec "$@"
