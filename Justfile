name    := "denv"
target  := arch() + "-apple-darwin"
nightly := "nightly-2026-02-23"

setup:
  rustup toolchain install {{ nightly }}
  rustup component add rust-src --toolchain {{ nightly }}
  prek install --install-hooks

build:
    cargo build

release:
    cargo clean -p {{ name }} --release --target {{ target }}
    RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
    cargo +{{ nightly }} build --release \
      -Z build-std=std \
      -Z build-std-features= \
      --target {{ target }}

install: release
    cp target/{{ target }}/release/{{ name }} ~/.local/bin/

pc:
  prek run --all-files
