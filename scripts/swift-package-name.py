#!/usr/bin/env python3
"""Read the package identity used to compile OrbitCore in this checkout."""

import json
import sys

with open(sys.argv[1]) as build_plan:
    commands = json.load(build_plan)["swiftCommands"].values()

names = set()
for command in commands:
    if command["moduleName"] == "OrbitCore":
        arguments = command["otherArguments"]
        names.add(arguments[arguments.index("-package-name") + 1])

if len(names) != 1:
    sys.exit("Expected one OrbitCore package identity in the Swift build plan")
print(names.pop())
