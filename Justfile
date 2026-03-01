target := arch() + "-apple-darwin"

build:
    cargo build

release:
    cargo clean -p denv --release --target {{ target }}
    RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
    cargo +nightly build --release \
      -Z build-std=std \
      -Z build-std-features= \
      --target {{ target }}

install: release
    cp target/{{ target }}/release/denv ~/.local/bin/
    grep -qF 'denv hook fish' ~/.config/fish/config.fish \
        || echo 'denv hook fish | source' >> ~/.config/fish/config.fish

setup:
  prek install --install-hooks

pc:
  prek run --all-files
