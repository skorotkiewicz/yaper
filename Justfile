# Justfile
# https://github.com/casey/just

[private]
default:
    @just --list

build:
    cargo build --release

run *args:
    cargo run -- {{args}}

fmt:
    cargo fmt
    cargo clippy --all-targets --all-features -- -D warnings
    # cargo shear --fix # first install shear: cargo install shear

check:
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings

install-hook:
    #!/usr/bin/env bash
    cat > .git/hooks/pre-commit << 'EOF'
    #!/bin/sh
    set -e
    echo "Running pre-commit quality checks..."
    just check
    EOF
    chmod +x .git/hooks/pre-commit
    echo "Pre-commit hook installation confirmed."

remove-hook:
    rm .git/hooks/pre-commit
    echo "Pre-commit hook uninstallation confirmed."

# Run unit tests
test: fmt
	cargo test

# Full end-to-end test: encode, decode JSON, decode tree text
test-e2e: build
	rm -rf test_dir
	mkdir -p test_dir/sub
	echo "hello" > test_dir/a.txt
	echo "rust" > test_dir/sub/b.rs
	./target/release/timber encode test_dir > test_enc.json
	./target/release/timber decode test_enc.json test_decoded
	@echo "=== JSON roundtrip ==="
	@cat test_decoded/test_dir/a.txt
	@cat test_decoded/test_dir/sub/b.rs
	tree test_dir > test_tree.txt
	./target/release/timber decode test_tree.txt test_tree_decoded
	@echo "=== tree-text decode ==="
	@find test_tree_decoded | sort
	tree -J test_dir > test_tree_j.json
	./target/release/timber decode test_tree_j.json test_j_decoded
	@echo "=== tree -J decode ==="
	@find test_j_decoded | sort
	@echo "All e2e tests passed!"

# Clean up all test artifacts including test_dir
clean-e2e:
	rm -f test_enc.json test_tree.txt test_tree_j.json
	rm -rf test_decoded test_tree_decoded test_j_decoded test_dir
	@echo "E2e artifacts cleaned."
