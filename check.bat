@echo off
setlocal

cd /d "%~dp0" || exit /b 1

where bun >nul 2>&1
if errorlevel 1 (
        echo Bun is required: https://bun.sh/ 1>&2
        exit /b 1
)

where cargo-machete >nul 2>&1
if errorlevel 1 (
        echo cargo-machete is required: cargo install cargo-machete 1>&2
        exit /b 1
)

echo Building and Testing Rust...
cargo build --all || exit /b 1
cargo test --all || exit /b 1

echo Running Rust Clippy...
cargo clippy --all --all-targets -- -D warnings || exit /b 1

echo Checking for unused Rust dependencies...
cargo machete || exit /b 1

echo Formatting checks...
cargo fmt --all -- --check || exit /b 1
call bunx @taplo/cli fmt --check || exit /b 1
call bunx prettier --check "**/*.md" || exit /b 1

cd /d "%~dp0ui" || exit /b 1

echo Running UI Quality checks...
call bun run quality || exit /b 1
cargo build --manifest-path "%~dp0Cargo.toml" --bin keeppeek || exit /b 1
call bun run test:e2e || exit /b 1
