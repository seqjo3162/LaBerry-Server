1) Отредачь turnserver.conf:
   - static-auth-secret

2) Запусти TURN:
   cd turn
   docker compose up -d

3) В сервер LaBerry добавь env:
   LB_TURN_URLS=turn:YOUR_PUBLIC_DOMAIN:3478?transport=udp,turn:YOUR_PUBLIC_DOMAIN:3478?transport=tcp
   LB_TURN_SECRET=ТВОЙ_static-auth-secret
   LB_TURN_TTL_SEC=3600

4) Клиенту выдавай ICE через GET /api/rtc/ice (Bearer token).
