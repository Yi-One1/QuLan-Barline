@echo off
rem Barline dev launcher: build latest code, then run the unpackaged exe.
rem Usage: double-click me anytime to check the newest changes (incremental build, fast if unchanged).
rem
rem v1.11.0 preview flow notes:
rem  - The exe is BUILT, not packaged: nothing is copied into deliverables\ here.
rem  - Version stays at the previous release number during acceptance; the bump and
rem    the packaged exe happen once, after you accept the result.
rem  - Always release profile, never debug: D2D + per-pixel transparency + 1s sampling
rem    are an order of magnitude slower in debug and would fake (or hide) stutter.
setlocal
cd /d "%~dp0"

rem A running instance owns the single-instance mutex, so a freshly built exe would
rem exit silently and you would keep looking at the OLD process.
tasklist /fi "imagename eq barline.exe" 2>nul | find /i "barline.exe" >nul
if not errorlevel 1 (
  echo [WARN] barline.exe is already running - that is the OLD build.
  echo        Exit it from the tray first ^(Quit^), then double-click me again.
  echo.
  pause
  exit /b 1
)

echo [1/3] Building latest code ...
powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Task build
if errorlevel 1 (
  echo.
  echo [FAIL] Build failed. Check errors above.
  pause
  exit /b 1
)

echo.
echo [2/3] Artifact that will be started:
for %%F in ("target\release\barline.exe") do echo        %%~fF
for %%F in ("target\release\barline.exe") do echo        modified: %%~tF   size: %%~zF bytes
git rev-parse --short HEAD
echo.

echo [3/3] Starting barline.exe ...
start "" "%~dp0target\release\barline.exe"
echo Started. If the widget does not show up, check the tray icon.
exit /b 0
