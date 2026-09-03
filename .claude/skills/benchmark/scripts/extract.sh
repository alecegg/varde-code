#!/usr/bin/env bash
# Parse-cost proxy: `extract` on ~/source/varde/src (474 files), no ast-grep
# equivalent — tracked as a standalone number, min-of-3.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
require_release_bin

path="${1:-$HOME/source/varde/src}"
ms=$(min_of_3 "$BIN" extract "$path")
printf 'extract\t%sms\n' "$ms"
