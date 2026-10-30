.PHONY: all build release test init start stop status exec clean check

ENTITLEMENTS := entitlements.plist
CLI_RELEASE := target/release/maclet
CLI_DEBUG := target/debug/maclet

all: release

check:
	cargo check --workspace

build:
	cargo build
	codesign --sign - --entitlements $(ENTITLEMENTS) --force $(CLI_DEBUG)

release:
	cargo build --release
	codesign --sign - --entitlements $(ENTITLEMENTS) --force $(CLI_RELEASE)

test:
	cargo test --workspace
	cargo test --package maclet-cli --test test_e2e_boot --no-run
	@for f in target/debug/deps/test_e2e_boot-*; do \
		if [ -x "$$f" ]; then \
			codesign --sign - --entitlements $(ENTITLEMENTS) --force "$$f" >/dev/null 2>&1; \
			"$$f"; \
		fi \
	done

init: release
	./$(CLI_RELEASE) machine init

start: release
	./$(CLI_RELEASE) machine start --daemon

status: release
	./$(CLI_RELEASE) machine status

stop: release
	./$(CLI_RELEASE) machine stop

exec: release
	./$(CLI_RELEASE) exec uname -a

clean:
	cargo clean
