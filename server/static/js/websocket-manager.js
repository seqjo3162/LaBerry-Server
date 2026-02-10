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
        
        console.log(`[WS ${currentConnectionId}] Connect requested`);
        
        // Если страница выгружается - не подключаемся
        if (this.isDisconnecting) {
            console.log(`[WS ${currentConnectionId}] Page is unloading, skipping connection`);
            return;
        }
        
        // Если уже подключаемся - ждем
        if (this.isConnecting) {
            console.log(`[WS ${currentConnectionId}] Already connecting, waiting...`);
            await new Promise(resolve => setTimeout(resolve, 100));
            if (currentConnectionId !== this.connectionId) {
                console.log(`[WS ${currentConnectionId}] Connection obsolete, aborting`);
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
            console.log(`[WS ${connectionId}] Closing previous connection`);
            this.ws.onclose = null;
            this.ws.close(1000, 'New connection requested');
            this.ws = null;
        }
        
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const host = window.location.hostname === 'localhost' ? 'localhost:5001' : '195.46.162.142:5001';
        const url = `${protocol}//${host}/ws?token=${token}`;
        console.log(`[WS ${connectionId}] Connecting to ${url}`);
        
        return new Promise((resolve, reject) => {
            const ws = new WebSocket(url);
            const timeout = setTimeout(() => {
                console.log(`[WS ${connectionId}] Connection timeout`);
                reject(new Error('Connection timeout'));
            }, 5000);
            
            ws.onopen = () => {
                clearTimeout(timeout);
                
                // Проверяем, не устарело ли соединение
                if (connectionId !== this.connectionId) {
                    console.log(`[WS ${connectionId}] Stale connection, closing`);
                    ws.close(1000, 'Stale connection');
                    reject(new Error('Stale connection'));
                    return;
                }
                
                console.log(`[WS ${connectionId}] ✅ Connected`);
                this.ws = ws;
                this.reconnectAttempts = 0;
                
                // Настраиваем обработчики
                this._setupHandlers(connectionId);
                
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
                console.log(`[WS ${connectionId}] Connection closed: ${event.code} ${event.reason}`);
                this._handleClose(event, connectionId);
            };
        });
    }
    
    _setupHandlers(connectionId) {
        if (!this.ws) return;
        
        this.ws.onmessage = (event) => {
            if (connectionId !== this.connectionId) {
                console.log(`[WS ${connectionId}] Ignoring message for stale connection`);
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
            console.log(`[WS] ✅ Connection established, connection_id: ${data.connection_id}, user_id: ${data.user_id}`);
            this.isAuthenticated = true;
            
            // Отправляем все ожидающие сообщения
            if (this.pendingMessages.length > 0) {
                console.log(`[WS] Sending ${this.pendingMessages.length} pending messages`);
                this.pendingMessages.forEach(msg => {
                    this.send(msg);
                });
                this.pendingMessages = [];
            }
            return;
        }
        
        // Обработка pong (ответ на наш ping)
        if (data.type === 'pong') {
            console.log(`[WS] Received pong, latency: ${Date.now() - data.t}ms`);
            return;
        }
        
        // Обработка takeover уведомления
        if (data.type === 'connection_taken_over') {
            console.log(`[WS] Connection taken over by new connection ${data.new_connection_id}`);
            this.disconnect('Connection taken over');
            return;
        }
        
        // Обработка join подтверждения
        if (data.type === 'joined') {
            console.log(`[WS] Joined room:`, data.room);
            // Здесь нужно вызвать колбэк для UI
            if (window.onChatJoined) {
                window.onChatJoined(data);
            }
            return;
        }
        
        // Обработка обычных сообщений
        if (data.type === 'message' || data.type === 'chat_message') {
            console.log(`[WS] Message received:`, data);
            // Здесь нужно вызвать колбэк для UI
            if (window.onChatMessage) {
                window.onChatMessage(data);
            }
            return;
        }
        
        // Обработка ошибок
        if (data.type === 'error') {
            console.error(`[WS] Server error: ${data.code}`, data);
            return;
        }
        
        // Обработка ping от сервера
        if (data.type === 'ping') {
            console.log('[WS] Received ping from server');
            // Отвечаем pong
            this.send({
                type: 'pong',
                t: Date.now()
            });
            return;
        }
        
        console.log('[WS] Unknown message:', data);
    }
    
    _handleClose(event, connectionId) {
        if (connectionId !== this.connectionId) {
            console.log(`[WS ${connectionId}] Ignoring close for stale connection`);
            return;
        }
        
        // Сбрасываем флаги
        this.isAuthenticated = false;
        
        // Очищаем интервалы
        if (this.pingInterval) {
            clearInterval(this.pingInterval);
            this.pingInterval = null;
        }
        
        // Если это не было ручное закрытие и страница не выгружается
        if (event.code !== 1000 && !this.isDisconnecting) {
            this._scheduleReconnect();
        }
    }
    
    _scheduleReconnect() {
        if (this.reconnectAttempts >= 10) {
            console.log('[WS] Max reconnect attempts reached');
            return;
        }
        
        this.reconnectAttempts++;
        const delay = Math.min(1000 * Math.pow(1.5, this.reconnectAttempts), this.maxReconnectDelay);
        
        console.log(`[WS] Reconnecting in ${delay}ms (attempt ${this.reconnectAttempts})`);
        
        this.reconnectTimer = setTimeout(() => {
            const token = localStorage.getItem('auth_token');
            if (token) {
                this.connect(token).catch(() => {
                    // Ошибка будет обработана в connect
                });
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
            console.log('[WS] Cannot send - WebSocket not connected');
            // Сохраняем сообщение в очередь, кроме ping
            if (data.type !== 'ping') {
                this.pendingMessages.push(data);
            }
            return false;
        }
        
        // Если не аутентифицированы, откладываем сообщения (кроме ping)
        if (!this.isAuthenticated && data.type !== 'ping') {
            console.log('[WS] Not authenticated yet, queuing message');
            this.pendingMessages.push(data);
            return false;
        }
        
        try {
            const message = JSON.stringify(data);
            this.ws.send(message);
            console.log(`[WS] Sent: ${data.type}`);
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
        console.log('[WS] Disconnecting:', reason);
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
            authenticаted: this.isAuthenticated,
            pendingMessages: this.pendingMessages.length,
            reconnectAttempts: this.reconnectAttempts
        };
    }
}

// Экспортируем синглтон
export const wsManager = new WebSocketManager();

// Глобальный хук для приложения
window.wsManager = wsManager;