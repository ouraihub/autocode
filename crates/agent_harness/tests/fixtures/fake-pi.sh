#!/bin/sh
# Minimal Pi RPC transcript for transport-level harness tests.
read -r state || exit 1
printf '%s\n' '{"type":"response","id":1,"success":true,"data":{"model":"fake-model","sessionFile":"/tmp/pi-session"}}'
read -r prompt || exit 1
printf '%s\n' '{"type":"agent_settled","sessionFile":"/tmp/pi-session"}'
