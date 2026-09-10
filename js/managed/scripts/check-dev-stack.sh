#!/bin/sh
# Offline compilation checks, run during the image build and on demand.
set -eu
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
node --version
npm --version
pnpm --version
uv --version
go version
swift --version
rustc --version
cargo --version
wasm-bindgen --version
cargo clippy --version
cargo fmt --version
printf 'console.log("node ok")\n' > hello.js
node hello.js
printf 'import Foundation\nprint("swift ok")\n' > hello.swift
swiftc hello.swift -o swift-hello
./swift-hello
printf 'package main\nimport "fmt"\nfunc main() { fmt.Println("go ok") }\n' > hello.go
GOCACHE="$work/go-cache" go build -o go-hello hello.go
./go-hello
printf 'fn main() { println!("rust ok"); }\n' > hello.rs
rustc hello.rs -o rust-hello
./rust-hello
rustc --target x86_64-unknown-linux-musl hello.rs -o musl-hello
./musl-hello
printf '#[no_mangle] pub extern "C" fn answer() -> u32 { 42 }\n' > wasm.rs
rustc --crate-type cdylib --target wasm32-unknown-unknown wasm.rs -o hello.wasm
test -s hello.wasm
python3 -c 'import ssl, sqlite3; print("python ok")'
