@echo off
setlocal
where pwsh.exe >nul 2>nul
if errorlevel 1 (
  echo PowerShell 7.2 or newer is required. Install it from https://aka.ms/powershell-release?tag=stable
  echo Then reopen the terminal and run this installer again.
  pause
  exit /b 1
)
pwsh.exe -NoProfile -File "%~dp0install-local-skill.ps1" %*
set "result=%errorlevel%"
if not "%result%"=="0" echo Installation did not complete. Review the error above; no settings were changed automatically.
pause
exit /b %result%
