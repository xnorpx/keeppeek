@echo off
setlocal

cd /d "%~dp0" || exit /b 1

where bun >nul 2>&1
if errorlevel 1 (
        echo Bun is required: https://bun.sh/ 1>&2
        exit /b 1
)

echo Formatting all files...
cargo fmt --all
call bunx @taplo/cli fmt
call bunx prettier --write "**/*.md"

cd /d "%~dp0ui" || exit /b 1
echo Formatting UI files...
call bun run lint:fix
call bun run format
