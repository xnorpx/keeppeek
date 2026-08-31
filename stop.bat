@echo off
setlocal

set "stopped_any="

sc.exe query KeepPeekService >nul 2>&1
if not errorlevel 1 (
                call :service_is_stopped
        if errorlevel 1 (
                echo Stopping KeepPeek service...
                sc.exe stop KeepPeekService >nul 2>&1
                call :wait_for_service 10
                if errorlevel 1 (
                        echo Unable to stop the KeepPeek service. Run this script as Administrator. 1>&2
                        exit /b 1
                )
                set "stopped_any=1"
        )
)

call :process_is_running
if not errorlevel 1 (
        echo Stopping KeepPeek...
        taskkill /IM keeppeek.exe >nul 2>&1
        call :wait_for_process 2
        if errorlevel 1 (
                echo KeepPeek did not stop promptly; forcing shutdown. 1>&2
                taskkill /F /IM keeppeek.exe >nul 2>&1
                call :wait_for_process 5
                if errorlevel 1 (
                        echo Unable to stop KeepPeek. Run this script as Administrator. 1>&2
                        exit /b 1
                )
        )
        set "stopped_any=1"
)

if defined stopped_any (
        echo KeepPeek stopped.
) else (
        echo KeepPeek is not running.
)
exit /b 0

:wait_for_service
set "attempts=%~1"
:wait_for_service_loop
call :service_is_stopped
if not errorlevel 1 exit /b 0
if "%attempts%"=="0" exit /b 1
timeout /t 1 /nobreak >nul 2>&1
set /a attempts-=1
goto wait_for_service_loop

:service_is_stopped
for /f "tokens=3" %%S in ('sc.exe query KeepPeekService ^| findstr /C:"STATE"') do (
                if "%%S"=="1" exit /b 0
)
exit /b 1

:process_is_running
tasklist /FI "IMAGENAME eq keeppeek.exe" /NH | findstr /I /B /C:"keeppeek.exe " >nul 2>&1
exit /b %errorlevel%

:wait_for_process
set "attempts=%~1"
:wait_for_process_loop
call :process_is_running
if errorlevel 1 exit /b 0
if "%attempts%"=="0" exit /b 1
timeout /t 1 /nobreak >nul 2>&1
set /a attempts-=1
goto wait_for_process_loop