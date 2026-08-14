@echo off
setlocal

cd /d "%~dp0ui" || exit /b 1

where bun >nul 2>&1
if errorlevel 1 (
	echo Bun is required: https://bun.sh/ 1>&2
	exit /b 1
)

call bun run quality || exit /b 1
cargo build --manifest-path "%~dp0Cargo.toml" --bin keeppeek || exit /b 1
call bun run test:e2e || exit /b 1