#!/bin/sh
set -euo pipefail

cd $(dirname "$0")

echo "Building binary..."
cargo build --release

tempfile=$(mktemp)

example() {
  echo
  echo "[$1] jj-analyze '$2'"
  ../target/release/jj-analyze --no-user-config --color=always "$2" \
    | tee $tempfile \
    | term-transcript capture --no-inputs --pure-svg --out "$1.svg" "jj-analyze '$2'"
  cat $tempfile
}

echo "Generating examples..."
example example-1     '@ | ancestors(immutable_heads().., 2) | trunk()'
example performance-1 'latest(empty())'
example performance-2 'latest(empty() & mutable())'
example performance-3 'latest(heads(empty() & mutable()))'

rm $tempfile
