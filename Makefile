NAME   := denv
UNAME  := $(shell uname -m)
TARGET := $(UNAME)-apple-darwin

.PHONY: setup build release install test test-ci pc bump-version

setup:
	rustup show active-toolchain
	prek install --install-hooks

build:
	cargo build

release:
	cargo clean -p $(NAME) --release --target $(TARGET)
	RUSTFLAGS="-Zlocation-detail=none -Zunstable-options -Cpanic=immediate-abort" \
	cargo build --release \
	  -Z build-std=std \
	  -Z build-std-features= \
	  --target $(TARGET)

install: release
	cp target/$(TARGET)/release/$(NAME) ~/.local/bin/

test:
	cargo test -- --test-threads=4

# So we don't do duplicate work (building both debug and release) in CI.
test-ci:
	cargo test --release -- --test-threads=4

pc:
	prek run --all-files

# Usage: make bump-version [V=x.y.z]
# Without V, increments the patch version.
bump-version:
ifndef V
	$(eval OLD := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml))
	$(eval V := $(shell echo "$(OLD)" | awk -F. '{printf "%d.%d.%d", $$1, $$2, $$3+1}'))
endif
	sed -i '' 's/^version = ".*"/version = "$(V)"/' Cargo.toml
	cargo check --quiet 2>/dev/null
	git add Cargo.toml Cargo.lock
	git commit -m "bump version to $(V)"
	git tag "release/$(V)"
