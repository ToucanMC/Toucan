.PHONY: server

CARGO_TARGET_DIR ?= target
SERVER_BINARY := toucan-server
TEST_DIR := .test

server:
	rm -rf -- "$(TEST_DIR)"
	mkdir -p -- "$(TEST_DIR)"
	cargo build --release --package "$(SERVER_BINARY)" --target-dir "$(CARGO_TARGET_DIR)"
	cp -- "$(CARGO_TARGET_DIR)/release/$(SERVER_BINARY)" "$(TEST_DIR)/$(SERVER_BINARY)"
