@echo off
rem Barline dev launcher: build latest code, then run the unpackaged exe.
rem Usage: double-click me anytime to check the newest changes (incremental build, fast if unchanged).
cd /d "%~dp0"
echo [1/2] Building latest code ...
powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Task build
if errorlevel 1 (
  echo.
  echo [FAIL] Build failed. Check errors above.
  pause
  exit /b 1
)
echo [2/2] Starting barline.exe ...
start "" "%~dp0target\release\barline.exe"
echo Started. If nothing appears, an instance may already be running (single-instance).
exit /b 0
