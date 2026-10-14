To start node:

```
bitcoind -datadir=/Volumes/Bluey/Bitcoin
```
`/Volumes/Bluey/Bitcoin/bitcoin.conf` must include `rest=1` - electrs 0.12+ fetches blocks via bitcoind's REST API (needs Bitcoin Core 31.0+).

In a separate window start electrs (native build, not Docker)
```
cd ~/repos/electrs
./run.sh
```
Log goes to the terminal. To run detached instead:
```
nohup ./run.sh > ~/.electrs/electrs.log 2>&1 &
```

Building
```
cargo build --release
```
Rust comes from Homebrew's rustup (PATH set in ~/.zshrc). RocksDB's build needs libclang from Xcode Command Line Tools - provided by `LIBCLANG_PATH` in `~/.cargo/config.toml` and the symlink `~/lib/libclang.dylib`.

To test RPC connection to bitcoind - expect an auth failure log in bitcoin debug.log
```
curl -u viv --data-binary '{"method":"getblockhash","params":[0],"id":1}' -H 'content-type: text/plain;' http://127.0.0.1:8332/
```

To test the REST API electrs uses
```
curl http://127.0.0.1:8332/rest/chaininfo.json
```

To test RPC connection for electrs
```
curl -X GET http://localhost:50001/
```

Check indexing progress
```
grep -o 'height=[0-9]*' ~/.electrs/electrs.log | tail -1
```

Troubleshooting
- Electrs doesn't respond when sparrow is connecting or doing a curl - it's because the database is being built. Need to keep waiting. Run Electrs with DEBUG log enabled.
- `Too many open files` when opening the index - macOS defaults to 256 open files; RocksDB keeps up to 256 open. `run.sh` raises it with `ulimit -n 4096`.
- `Can't assign requested address (os error 49)` during a full reindex - macOS runs out of ephemeral ports because electrs makes thousands of REST requests/sec on early blocks. Before reindexing (resets on reboot):
  ```
  sudo sysctl -w net.inet.tcp.msl=1000
  sudo sysctl -w net.inet.ip.portrange.first=10000
  ```
- Upgrading to 0.12 (Oct 2026) required a full reindex (~10h). Old index format is not compatible.

Old Docker setup (pre-0.12, no longer used)
- run.sh used to start the electrs docker container (`--network host`, Docker Desktop 'Enable host networking' enabled); from within the container bitcoind is at `host.docker.internal` if that setting is off.
- Electrs had trouble with the .cookie file in Docker - bitcoind recreates it and the volume mount didn't refresh; an `ls -al` in the bitcoin folder fixed it.
