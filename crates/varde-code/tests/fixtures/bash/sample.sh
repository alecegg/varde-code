#!/usr/bin/env bash
# Coverage-parity fixture for Bash: exercises every kind in REQUIRED_KINDS
# (Function, Variable, Export, Call, Literal, ControlFlow) plus Import.
set -euo pipefail

# Import: `source`/`.` both map to Import (not Call).
source ./lib.sh
. ./other.sh

# Variables: plain, quoted string, number, and a `local`/`declare` declaration.
name="world"
count=42
readonly PI=3
declare -i total=0

# Export: a real Bash export builtin -> Export.
export GREETING="hi"
export PATH_HINT

# Function (POSIX `foo()` form).
greet() {
    local who="$1"
    echo "hello $who"
    return 0
}

# Function (`function name` keyword form).
function tally {
    for n in 1 2 3; do
        total=$((total + n))
    done
    while true; do
        break
    done
    until false; do
        continue
    done
    echo "$total"
}

# Control flow at top level + plain command Calls.
if [ "$count" = "42" ]; then
    greet "$name"
elif [ "$count" = "0" ]; then
    echo "zero"
else
    echo "other"
fi

case "$name" in
    world) echo "the world" ;;
    *) echo "someone" ;;
esac

# Plain command Calls.
grep -r foo .
tally
