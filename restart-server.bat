@echo off
chcp 65001 >nul
title Restart LaBerry Server

net session >nul 2>&1
if %errorlevel% neq 0 (
    echo Requesting admin rights...
    powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "Start-Process '%~f0' -Verb RunAs"
    exit /b
)

setlocal

echo === Restart LaBerry Server as Admin ===

echo Stopping by ports 5001/5002...

for %%P in (5001 5002) do (
    for /f "tokens=5" %%A in ('netstat -ano ^| findstr /R /C:":%%P .*LISTENING"') do (
        echo Killing PID %%A on port %%P
        taskkill /F /T /PID %%A >nul 2>nul
    )
)

echo Stopping known server exe names...
taskkill /F /T /IM laberry_server_bin.exe >nul 2>nul
taskkill /F /T /IM laberry.exe >nul 2>nul

timeout /t 2 /nobreak >nul


rem === LaBerry AI / LM Studio ===
set "LB_AI_ENABLED=true"
set "LB_AI_BASE_URL=http://127.0.0.1:1234/v1"
set "LB_AI_MODEL=qwen_qwen3-4b-instruct-2507"
set "LB_AI_USER_NAME=Gemka III"

rem === Secret dev-web link for admin panel ===
rem Fill this before using the admin button "Кинуть dev-web".
rem Example: set "LB_DEV_WEB_URL=https://dev.example.local"
set "LB_DEV_WEB_URL="

echo Starting server...
start "LaBerry Server" powershell.exe -NoProfile -ExecutionPolicy Bypass -File "D:\LaBerry-Server\run-server.ps1" -Admin

echo Done.
timeout /t 1 /nobreak >nul
exit