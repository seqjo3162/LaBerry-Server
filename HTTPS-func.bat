@echo off
chcp 65001 >nul
title Caddy HTTPS Launcher

echo ========================================
echo   Остановка старых процессов Caddy...
echo ========================================
taskkill /F /IM caddy.exe >nul 2>&1
timeout /t 1 /nobreak >nul

echo Запуск Caddy HTTPS...
start "Caddy Server" powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0run-https.ps1"
exit