#!/usr/bin/env bash
set -euo pipefail

cargo test -p context-storage --test segment_format
