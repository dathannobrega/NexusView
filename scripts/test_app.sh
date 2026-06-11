#!/usr/bin/env bash
# Compile-and-run unit tests for the app's pure-Swift logic.
#
# The project builds with Command Line Tools only (no Xcode.app), which ships
# no XCTest — so app-side tests are plain executables: each harness under
# app/Tests/<Name>/main.swift is compiled together with the source files it
# tests and must exit 0.
set -euo pipefail
cd "$(dirname "$0")/.."

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "== TimestampParserTests =="
swiftc -O \
    app/Sources/NexusView/TimestampParser.swift \
    app/Tests/TimestampParserTests/main.swift \
    -o "$tmp/timestamp_parser_tests"
"$tmp/timestamp_parser_tests"
