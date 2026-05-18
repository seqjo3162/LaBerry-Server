let WS_DEBUG = false;
try {
    WS_DEBUG = typeof window !== 'undefined' && (
        window.DEBUG_WS === true || localStorage.getItem('lb_debug_ws') === '1'
    );
} catch (_) {}
const wsLog = (...args) => {
    if (WS_DEBUG) console.log(...args);
};

wsLog('[WS] websocket-manager loaded (v2026-02-18)');

// NOTE: do NOT log full tokens/URLs (tokens in query are sensitive).
function _lbDecodeJwtPayload(token) {
    try {
        const parts = String(token || '').split('.');
        if (parts.length < 2) return null;
        const b64 = parts[1].replace(/-/g, '+').replace(/_/g, '/');
        const pad = b64.length % 4 ? '='.repeat(4 - (b64.length % 4)) : '';
        const json = atob(b64 + pad);
        return JSON.parse(json);
    } catch (_) {
        return null;
    }
}

function _lbIsJwtExpired(token, skewSec = 15) {
    const p = _lbDecodeJwtPayload(token);
    const exp = p && typeof p.exp === 'number' ? p.exp : null;
    if (!exp) return false; // can't determine -> don't block
    const nowSec = Math.floor(Date.now() / 1000);
    return exp <= (nowSec + skewSec);
}

// /static/js/websocket-manager.js

class WebSocketManager {
    constructor() {
        this.ws = null;
        this.reconnectAttempts = 0;
        this.maxReconnectDelay = 10000;
        this.reconnectTimer = null;
        this.pingInterval = null;
        this.isConnecting = false;
        this.isDisconnecting = false;
        this.connectionId = 0;
        this.pendingMessages = [];
        this.isAuthenticated = false;
        
        // Статистика для отладки
        this.stats = {
            created: 0,
            connected: 0,
            errors: 0
        };
    }
    
    // Основной метод подключения с защитой от гонки
    async connect(token) {
        this.stats.created++;
        const currentConnectionId = ++this.connectionId;

        // stop immediately if token is missing/expired (prevents spam reconnect + ExpiredSignature logs)
        if (!token) {
            console.warn(`[WS ${currentConnectionId}] No token, aborting`);
            return;
        }
        if (_lbIsJwtExpired(token)) {
            console.warn(`[WS ${currentConnectionId}] Token expired, aborting`);
            try { localStorage.removeItem('auth_token'); } catch (_) {}
            try { localStorage.removeItem('refresh_token'); } catch (_) {}
            return;
        }
        
        wsLog(`[WS ${currentConnectionId}] Connect requested`);
        
        // Если страница выгружается - не подключаемся
        if (this.isDisconnecting) {
            wsLog(`[WS ${currentConnectionId}] Page is unloading, skipping connection`);
            return;
        }
        
        // Если уже подключаемся - ждем
        if (this.isConnecting) {
            wsLog(`[WS ${currentConnectionId}] Already connecting, waiting...`);
            await new Promise(resolve => setTimeout(resolve, 100));
            if (currentConnectionId !== this.connectionId) {
                wsLog(`[WS ${currentConnectionId}] Connection obsolete, aborting`);
                return;
            }
        }
        
        this.isConnecting = true;
        this.isAuthenticated = false;
        
        try {
            await this._connectInternal(token, currentConnectionId);
            this.stats.connected++;
        } catch (error) {
            this.stats.errors++;
            console.error(`[WS ${currentConnectionId}] Connection failed:`, error);
            throw error;
        } finally {
            this.isConnecting = false;
        }
    }
    
    async _connectInternal(token, connectionId) {
        // Закрываем предыдущее соединение
        if (this.ws && this.ws.readyState !== WebSocket.CLOSED) {
            wsLog(`[WS ${connectionId}] Closing previous connection`);
            this.ws.onclose = null;
            this.ws.close(1000, 'New connection requested');
            this.ws = null;
        }
        
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        // Всегда подключаемся к текущему хосту (чтобы работало с доменом/проксированием)
        const host = window.location.host;
        const url = `${protocol}//${host}/ws`;
        const safeUrl = `${protocol}//${host}/ws`;
        wsLog(`[WS ${connectionId}] Connecting to ${safeUrl}`);
        
        return new Promise((resolve, reject) => {
            const ws = new WebSocket(url);
            const timeout = setTimeout(() => {
                wsLog(`[WS ${connectionId}] Connection timeout`);
                reject(new Error('Connection timeout'));
            }, 5000);
            
            ws.onopen = () => {
                clearTimeout(timeout);
                
                // Проверяем, не устарело ли соединение
                if (connectionId !== this.connectionId) {
                    wsLog(`[WS ${connectionId}] Stale connection, closing`);
                    ws.close(1000, 'Stale connection');
                    reject(new Error('Stale connection'));
                    return;
                }
                
                wsLog(`[WS ${connectionId}] ✅ Connected`);
                this.ws = ws;
                this.reconnectAttempts = 0;
                
                // Настраиваем обработчики
                this._setupHandlers(connectionId);
                
                // Authenticate (token sent as WS message, avoids URL log exposure)
                try {
                    ws.send(JSON.stringify({ type: 'auth', token }));
                } catch (_) {}

                // Запускаем ping
                this._startPing();
                
                resolve();
            };
            
            ws.onerror = (error) => {
                clearTimeout(timeout);
                console.error(`[WS ${connectionId}] Connection error:`, error);
                reject(error);
            };
            
            ws.onclose = (event) => {
                clearTimeout(timeout);
                wsLog(`[WS ${connectionId}] Connection closed: ${event.code} ${event.reason}`);
                this._handleClose(event, connectionId);
            };
        });
    }
    
    _setupHandlers(connectionId) {
        if (!this.ws) return;
        
        this.ws.onmessage = (event) => {
            if (connectionId !== this.connectionId) {
                wsLog(`[WS ${connectionId}] Ignoring message for stale connection`);
                return;
            }
            
            try {
                const data = JSON.parse(event.data);
                this._handleMessage(data);
            } catch (error) {
                console.error(`[WS ${connectionId}] Message parse error:`, error);
            }
        };
    }
    
    _handleMessage(data) {
        // Обработка welcome-сообщения
        if (data.type === 'connected') {
            wsLog(`[WS] ✅ Connection established, connection_id: ${data.connection_id}, user_id: ${data.user_id}`);
            this.isAuthenticated = true;
            
            // Отправляем все ожидающие сообщения
            if (this.pendingMessages.length > 0) {
                wsLog(`[WS] Sending ${this.pendingMessages.length} pending messages`);
                this.pendingMessages.forEach(msg => {
                    this.send(msg);
                });
                this.pendingMessages = [];
            }
            return;
        }
        
        // Обработка pong (ответ на наш ping)
        if (data.type === 'pong') {
            wsLog(`[WS] Received pong, latency: ${Date.now() - data.t}ms`);
            return;
        }

        if (data.type === 'force_logout' || data.type === 'token_invalidated') {
            console.warn('[WS] Session ended:', data.reason || data.code || data.type);
            try { localStorage.removeItem('auth_token'); } catch (_) {}
            try { localStorage.removeItem('refresh_token'); } catch (_) {}
            this.disconnect('Session ended');
            if (typeof window !== 'undefined' && window.location.pathname !== '/') {
                window.location.href = '/';
            }
            return;
        }
        
        // Обработка takeover уведомления
        if (data.type === 'connection_taken_over') {
            wsLog(`[WS] Connection taken over by new connection ${data.new_connection_id}`);
            this.disconnect('Connection taken over');
            return;
        }
        
        // Обработка join подтверждения
        if (data.type === 'joined') {
            wsLog(`[WS] Joined room:`, data.room);
            // Здесь нужно вызвать колбэк для UI
            if (window.onChatJoined) {
                window.onChatJoined(data);
            }
            return;
        }
        
        // Обработка обычных сообщений + событий чата
        if (data.type === 'message' || data.type === 'chat_message' || data.type === 'reaction' || data.type === 'message_deleted') {
            wsLog(`[WS] Message received:`, data);
            // Здесь нужно вызвать колбэк для UI
            if (window.onChatMessage) {
                window.onChatMessage(data);
            }
            return;
        }
        
        // Обработка ошибок
        if (data.type === 'error') {
            console.error(`[WS] Server error: ${data.code}`, data);

            // Forward to optional handlers (voice/chat may need to react on errors)
            if (window.onWsError) {
                try { window.onWsError(data); } catch (e) { console.error('[WS] onWsError error', e); }
            }
            if (window.onChatError) {
                try { window.onChatError(data); } catch (e) { console.error('[WS] onChatError error', e); }
            }
            if (window.onVoiceEvent) {
                try { window.onVoiceEvent(data); } catch (e) { console.error('[WS] onVoiceEvent error', e); }
            }
            return;
        }
        
        // Обработка ping от сервера
        if (data.type === 'ping') {
            wsLog('[WS] Received ping from server');
            // Отвечаем pong
            this.send({
                type: 'pong',
                t: Date.now()
            });
            return;
        }
        
        // DM call events
        if (data && typeof data.type === 'string' && data.type.startsWith('dm_call_')) {
            if (window.onDmCallEvent) {
                try { window.onDmCallEvent(data); } catch (e) { console.error('[WS] onDmCallEvent error', e); }
            } else if (window.onWsMessage) {
                try { window.onWsMessage(data); } catch (e) { console.error('[WS] onWsMessage error', e); }
            } else {
                wsLog('[WS] DM call event (no handler):', data);
            }
            return;
        }

        // Voice/WebRTC events
        if (data && typeof data.type === 'string' && (data.type.startsWith('voice_') || data.type.startsWith('rtc_'))) {
            if (window.onVoiceEvent) {
                try { window.onVoiceEvent(data); } catch (e) { console.error('[WS] onVoiceEvent error', e); }
            } else {
                wsLog('[WS] Voice event (no handler):', data);
            }
            return;
        }

        // Global fallback hook (for future features)
        if (window.onWsMessage) {
            try { window.onWsMessage(data); } catch (e) { console.error('[WS] onWsMessage error', e); }
            return;
        }

        wsLog('[WS] Unknown message:', data);
    }
    
    _handleClose(event, connectionId) {
        if (connectionId !== this.connectionId) {
            wsLog(`[WS ${connectionId}] Ignoring close for stale connection`);
            return;
        }
        
        // Сбрасываем флаги
        this.isAuthenticated = false;
        
        // Очищаем интервалы
        if (this.pingInterval) {
            clearInterval(this.pingInterval);
            this.pingInterval = null;
        }

        // Fatal auth/permission close codes (server may close with Policy Violation).
        if (event.code === 1008 || event.code === 4001) {
            console.warn('[WS] Fatal close, stopping reconnect:', event.code, event.reason);
            try { localStorage.removeItem('auth_token'); } catch (_) {}
            try { localStorage.removeItem('refresh_token'); } catch (_) {}
            return;
        }
        
        // Если это не было ручное закрытие и страница не выгружается
        if (event.code !== 1000 && !this.isDisconnecting) {
            this._scheduleReconnect();
        }
    }
    
    _scheduleReconnect() {
        if (this.reconnectAttempts >= 10) {
            wsLog('[WS] Max reconnect attempts reached');
            return;
        }
        
        this.reconnectAttempts++;
        const delay = Math.min(1000 * Math.pow(1.5, this.reconnectAttempts), this.maxReconnectDelay);
        
        wsLog(`[WS] Reconnecting in ${delay}ms (attempt ${this.reconnectAttempts})`);
        
        this.reconnectTimer = setTimeout(() => {
            const token = localStorage.getItem('auth_token');
            if (token && !_lbIsJwtExpired(token)) {
                this.connect(token).catch(() => {
                    // Ошибка будет обработана в connect
                });
            } else {
                if (token) {
                    console.warn('[WS] Token expired, stopping reconnect');
                    try { localStorage.removeItem('auth_token'); } catch (_) {}
            try { localStorage.removeItem('refresh_token'); } catch (_) {}
                }
            }
        }, delay);
    }
    
    _startPing() {
        if (this.pingInterval) {
            clearInterval(this.pingInterval);
        }
        
        this.pingInterval = setInterval(() => {
            if (this.ws && this.ws.readyState === WebSocket.OPEN && this.isAuthenticated) {
                this.send({
                    type: 'ping',
                    timestamp: Date.now()
                });
            }
        }, 25000); // 25 секунд
    }
    
    send(data) {
        // Если соединение не установлено или не аутентифицировано
        if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
            wsLog('[WS] Cannot send - WebSocket not connected');
            // Сохраняем сообщение в очередь, кроме ping
            if (data.type !== 'ping') {
                this.pendingMessages.push(data);
            }
            return false;
        }
        
        // Если не аутентифицированы, откладываем сообщения (кроме ping)
        if (!this.isAuthenticated && data.type !== 'ping') {
            wsLog('[WS] Not authenticated yet, queuing message');
            this.pendingMessages.push(data);
            return false;
        }
        
        try {
            const message = JSON.stringify(data);
            this.ws.send(message);
            wsLog(`[WS] Sent: ${data.type}`);
            return true;
        } catch (error) {
            console.error('[WS] Send error:', error);
            return false;
        }
    }
    
    joinRoom(roomId) {
        return this.send({
            type: 'join_chat',
            data: {
                chat_id: roomId
            }
        });
    }
    
    sendMessage(roomId, content) {
        return this.send({
            type: 'send_message',
            data: {
                chat_id: roomId,
                content: content
            }
        });
    }
    
    disconnect(reason = 'User disconnect') {
        wsLog('[WS] Disconnecting:', reason);
        this.isDisconnecting = true;
        this.isAuthenticated = false;
        
        // Очищаем таймеры
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = null;
        }
        
        if (this.pingInterval) {
            clearInterval(this.pingInterval);
            this.pingInterval = null;
        }
        
        // Закрываем соединение
        if (this.ws) {
            this.ws.onclose = null;
            if (this.ws.readyState === WebSocket.OPEN) {
                this.ws.close(1000, reason);
            }
            this.ws = null;
        }
    }
    
    // Утилита для проверки состояния
    getStatus() {
        return {
            connected: this.ws && this.ws.readyState === WebSocket.OPEN,
            authenticated: this.isAuthenticated,
            pendingMessages: this.pendingMessages.length,
            reconnectAttempts: this.reconnectAttempts
        };
    }

    // Для совместимости с app.js (там ожидается property)
    get isConnected() {
        return !!(this.ws && this.ws.readyState === WebSocket.OPEN);
    }
}

// Экспортируем синглтон
export const wsManager = new WebSocketManager();

// Глобальный хук для приложения
window.wsManager = wsManager;
