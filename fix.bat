@echo off
setlocal

cd /d "%~dp0" || exit /b 1

where bun >nul 2>&1
if errorlevel 1 (
        echo Bun is required: https://bun.sh/ 1>&2
        exit /b 1
)

if defined KEEPPEEK_PYTHON (
        set "PYTHON_CMD=%KEEPPEEK_PYTHON%"
) else (
        where python >nul 2>&1
        if errorlevel 1 (
                echo Python 3.12 is required. 1>&2
                exit /b 1
        )
        set "PYTHON_CMD=python"
)

set "REQUIREMENTS=examples\object_detection_service\requirements.txt"

rem Requirements are intentionally unpinned, so refresh them to the versions CI will resolve.
echo Updating Python requirements...
"%PYTHON_CMD%" -m pip install --quiet --upgrade -r "%REQUIREMENTS%" >nul 2>&1
rem Externally managed interpreters reject installs without --break-system-packages.
if errorlevel 1 "%PYTHON_CMD%" -m pip install --quiet --upgrade --break-system-packages -r "%REQUIREMENTS%"

"%PYTHON_CMD%" -c "import black" >nul 2>&1
if errorlevel 1 (
        echo Black is required: "%PYTHON_CMD%" -m pip install -r "%REQUIREMENTS%" 1>&2
        exit /b 1
)

echo Formatting Rust files...
cargo fmt --all || exit /b 1

echo Formatting TOML files...
call bunx @taplo/cli fmt || exit /b 1

echo Formatting Python files...
"%PYTHON_CMD%" -m black --config examples\object_detection_service\pyproject.toml . || exit /b 1

cd /d "%~dp0ui" || exit /b 1
echo Formatting Markdown files...
call bunx prettier --write "../**/*.md" || exit /b 1

echo Formatting UI files...
call bun run lint:fix || exit /b 1
call bun run format || exit /b 1
