#!/bin/bash

#docker run \
#--network host \
#--volume /Volumes/Bluey/Bitcoin:/data/bitcoin:ro \
#             --volume /Volumes/Bluey/electrs:/data/electrs \
#             --env ELECTRS_DB_DIR=/data/electrs/db \
#             --rm -i -t electrs-app
export RUST_BACKTRACE=full
# RocksDB (bindex) keeps up to 256 .sst files open; the macOS default limit is 256
ulimit -n 4096
./target/release/electrs --conf ./electrs.conf