# provision — build and install.
#
# D1: one static binary per target, built natively on the machine that will
# run it. The one exception is Windows, which has no toolchain of its own in
# this fleet and is cross-compiled from Linux (D1 amendment).

# Native release build.
build:
    cargo build --release

# Windows binary, cross-compiled with mingw-w64. Needs the target and the
# linker: `rustup target add x86_64-pc-windows-gnu` and mingw-w64.
windows:
    cargo build --release --target x86_64-pc-windows-gnu

# Copy the native binary onto PATH. Deliberately not `cargo install`: that
# rebuilds into its own target dir and would double the build time for a
# binary `build` has already produced.
install: build
    mkdir -p ~/.local/bin
    cp target/release/provision ~/.local/bin/provision
