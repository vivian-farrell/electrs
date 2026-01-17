To start node:

```
bitcoind -datadir=/Volumes/Bluey/Bitcoin 
```
In a separate window start electrum
```
cd ~/repos/electrs
. ./run.sh # runs the electrs docker container and presents ssh session
```

From inside container
```
cd /data/electrs
. ./run.sh # runs electrs
```

To test RPC connection to bitcoind - expect an auth failure log in bitcoin debug.log
```
curl -u viv --data-binary '{"method":"getblockhash","params":[0],"id":1}' -H 'content-type: text/plain;' http://127.0.0.1:8332/
```

To test RPC connection for electrs
```
curl -X GET http://localhost:50001/
```

Notes on networking
- Docker container runs with network=host
- Docker desktop has 'Enable host networking' enabled
- From within container, have to use `host.docker.internal` to address bitcoind if the above docker desktop setting is not set.

Troubleshooting
- Electrs has trouble with the .cookie file. - This is because bitcoind has recreated the cookie file while electrs is running and because of a weird issue with the volume mount not refreshing. It's solved by navigating to the bitcoin folder and doing an ls -al. Not sure exactly why.
- Electrs doesn't respond when sparrow is connecting or doing a curl - it's because the database is being built. Need to keep waiting. Run Electrs with DEBUG log enabled.
- 
