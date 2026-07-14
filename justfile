default: all

fmt:
    cargo fmt --all

check:
    cargo check --workspace

clippy:
    cargo clippy --workspace -- -D warnings

# The gate is 98 rather than 100 because grcov measures #[cfg(test)]
# modules too, and their failure paths — panic! arms and `?` error
# branches — never run in a passing suite. Library code is either
# covered or carries an explicit cov-excl-line with reasoning.
coverage:
    COVERAGE_THRESHOLD=98 ./bin/coverage

mutants:
    #!/usr/bin/env bash
    set -uo pipefail
    cargo mutants --timeout-multiplier 3 -j4
    rc=$?
    # 0 = all caught, 3 = timeouts (infinite loops from mutants, still caught).
    if [ "$rc" -eq 0 ] || [ "$rc" -eq 3 ]; then
        exit 0
    fi
    exit "$rc"

all: fmt clippy coverage

install:
    cargo install --locked --path .
