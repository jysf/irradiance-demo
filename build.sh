#!/bin/sh
# Build the WebAssembly package into web/pkg.
# Uses rustup's stable toolchain explicitly: a Homebrew rustc earlier on PATH
# has no wasm32 standard library and fails with "can't find crate for std".
set -eu
cd "$(dirname "$0")/demo-core"
TOOLCHAIN_BIN="$(dirname "$(rustup which --toolchain stable rustc)")"
PATH="$TOOLCHAIN_BIN:$PATH" wasm-pack build --release --target web --out-dir ../web/pkg --no-typescript
rm -f ../web/pkg/.gitignore ../web/pkg/package.json
