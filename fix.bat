@echo off
setlocal

cd /d "%~dp0" || exit /b 1

where bun >nul 2>&1
if errorlevel 1 (
        echo Bun is required: https://bun.sh/ 1>&2
        exit /b 1
)

echo Formatting Rust files...
cargo fmt --all || exit /b 1

echo Formatting TOML files...
call bunx @taplo/cli fmt || exit /b 1

echo Formatting Markdown files...
call bunx prettier --write "**/*.md" || exit /b 1

cd /d "%~dp0ui" || exit /b 1
echo Formatting UI files...
call bun run lint:fix || exit /b 1
call bun run format || exit /b 1
