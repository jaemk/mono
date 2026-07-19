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

# mapour migrations only run once the site is enabled and its db exists
if [ "${MAPOUR_ENABLED:-false}" = "true" ]; then
    pushd migrations/mapour
    migrant setup
    migrant list
    migrant apply -a || true
    popd
fi

exec "$@"
