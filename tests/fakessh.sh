#!/bin/sh
# Stands in for ssh in the tests: ignores the host and runs the command
# here, so the remote code path (server mode, the pipes, the relayed
# stderr) runs without a network.  usage: fakessh.sh HOST COMMAND
shift
exec sh -c "$*"
