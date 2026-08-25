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

where cargo-nextest >nul 2>&1
if errorlevel 1 (
        echo cargo-nextest is required: cargo install cargo-nextest 1>&2
        exit /b 1
)

if defined KEEPPEEK_PYTHON (
        set "PYTHON_CMD=%KEEPPEEK_PYTHON%"
) else if exist "%~dp0example\object_detection_service\.venv\Scripts\python.exe" (
        set "PYTHON_CMD=%~dp0example\object_detection_service\.venv\Scripts\python.exe"
) else (
        where python >nul 2>&1
        if errorlevel 1 (
                echo Python 3.12 is required. 1>&2
                exit /b 1
        )
        set "PYTHON_CMD=python"
)

"%PYTHON_CMD%" -c "import black" >nul 2>&1
if errorlevel 1 (
        echo Black is required: python -m pip install -r example\object_detection_service\requirements.txt 1>&2
        exit /b 1
)

echo Building and Testing Rust...
cargo build --all || exit /b 1
cargo nextest run --all || exit /b 1

echo Running Rust Clippy...
cargo clippy --all --all-targets -- -D warnings || exit /b 1

echo Checking for unused Rust dependencies...
cargo machete || exit /b 1

echo Formatting checks...
cargo fmt --all -- --check || exit /b 1
call bunx @taplo/cli fmt --check || exit /b 1
"%PYTHON_CMD%" -m black --check --config example\object_detection_service\pyproject.toml . || exit /b 1

cd /d "%~dp0ui" || exit /b 1
call bunx prettier --check "../**/*.md" || exit /b 1

echo Running UI Quality checks...
call bun run quality:check || exit /b 1
call bun run test:e2e || exit /b 1
