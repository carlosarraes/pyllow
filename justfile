build:
    cargo build --release -p pyllow-cli
    mkdir -p ~/.local/bin
    cp target/release/pyllow ~/.local/bin/
    @echo "Installed pyllow to ~/.local/bin/"

install: build

test:
    cargo test --workspace

check:
    cargo check --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all --check
