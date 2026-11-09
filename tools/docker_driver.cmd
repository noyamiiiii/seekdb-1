@echo off
if not defined MVDB_IMAGE set "MVDB_IMAGE=minivectordb:round2-candidate"
docker run --rm -i --network none --cpus=2 --memory=128m --memory-swap=128m --mount "type=bind,source=%~2,target=/data" %MVDB_IMAGE%
