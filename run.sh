#!/bin/bash

#docker run \
#--network host \
#--volume /Volumes/Bluey/Bitcoin:/data/bitcoin:ro \
#             --volume /Volumes/Bluey/electrs:/data/electrs \
#             --env ELECTRS_DB_DIR=/data/electrs/db \
#             --rm -i -t electrs-app
export RUST_BACKTRACE=full
./target/release/electrs --conf ./electrs.conf