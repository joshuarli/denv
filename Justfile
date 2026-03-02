target  := arch() + "-apple-darwin"
nightly := "nightly-2026-02-23"

build:
    cargo build

release:
    cargo clean -p denv --release --target {{ target }}
    RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
    cargo +{{ nightly }} build --release \
      -Z build-std=std \
      -Z build-std-features= \
      --target {{ target }}

install: release
    cp target/{{ target }}/release/denv ~/.local/bin/

setup:
  prek install --install-hooks

pc:
  prek run --all-files
