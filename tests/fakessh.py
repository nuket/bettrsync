#!/usr/bin/env python3
# bettrsync
# Copyright (c) 2026 Max Vilimpoc
# SPDX-License-Identifier: MIT
# Stands in for ssh in the tests, on Windows as on Linux: ignores the
# host and runs the command here through the system's shell (cmd on
# Windows, sh elsewhere), as sshd would, with the pipes untouched.
# usage: fakessh.py HOST COMMAND
import subprocess
import sys

sys.exit(subprocess.call(sys.argv[2], shell=True))
