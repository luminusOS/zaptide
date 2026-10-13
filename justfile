# Run inside the toolbox that has the GTK4 and libadwaita development files.

default:
    @just --list

run:
    cargo run

fmt:
    cargo fmt

# The same checks CI runs.
check:
    cargo fmt --check
    cargo clippy --locked --all-targets -- -D warnings
    cargo test --locked --all-targets

test:
    cargo test

clean:
    cargo clean
