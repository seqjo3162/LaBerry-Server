echo Starting HTTPS...
start "LaBerry Server" powershell.exe -NoProfile -ExecutionPolicy Bypass -File "D:\LaBerry-Server\run-https.ps1"
echo Done.
timeout /t 1 /nobreak >nul
exit