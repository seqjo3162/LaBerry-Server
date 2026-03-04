console.log('[WS] websocket-manager loaded (v2026-02-18)');
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
    if (!exp) return false;
    const nowSec = Math.floor(Date.now() / 1000);
    return exp <= (nowSec + skewSec);
}

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
        this.stats = {
            created: 0,
            connected: 0,
            errors: 0
        };
    }
    
    async connect(token) {
        this.stats.created++;
        const currentConnectionId = ++this.connectionId;

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
        
        console.log(`[WS ${currentConnectionId}] Connect requested`);
        
        if (this.isDisconnecting) {
            console.log(`[WS ${currentConnectionId}] Page is unloading, skipping connection`);
            return;
        }
        
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
        if (this.ws && this.ws.readyState !== WebSocket.CLOSED) {
            console.log(`[WS ${connectionId}] Closing previous connection`);
            this.ws.onclose = null;
            this.ws.close(1000, 'New connection requested');
            this.ws = null;
        }
        
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const host = window.location.host;
        const url = `${protocol}//${host}/ws`;
        const safeUrl = `${protocol}//${host}/ws`;
        console.log(`[WS ${connectionId}] Connecting to ${safeUrl}`);
        
        return new Promise((resolve, reject) => {
            const ws = new WebSocket(url);
            const timeout = setTimeout(() => {
                console.log(`[WS ${connectionId}] Connection timeout`);
                reject(new Error('Connection timeout'));
            }, 5000);
            
            ws.onopen = () => {
                clearTimeout(timeout);
                
                if (connectionId !== this.connectionId) {
                    console.log(`[WS ${connectionId}] Stale connection, closing`);
                    ws.close(1000, 'Stale connection');
                    reject(new Error('Stale connection'));
                    return;
                }
                
                console.log(`[WS ${connectionId}] ✅ Connected`);
                this.ws = ws;
                this.reconnectAttempts = 0;
                this._setupHandlers(connectionId);
                
                try {
                    ws.send(JSON.stringify({ type: 'auth', token }));
                } catch (_) {}

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
        if (data.type === 'connected') {
            console.log(`[WS] ✅ Connection established, connection_id: ${data.connection_id}, user_id: ${data.user_id}`);
            this.isAuthenticated = true;
            
            if (this.pendingMessages.length > 0) {
                console.log(`[WS] Sending ${this.pendingMessages.length} pending messages`);
                this.pendingMessages.forEach(msg => {
                    this.send(msg);
                });
                this.pendingMessages = [];
            }
            return;
        }
        
        if (data.type === 'pong') {
            console.log(`[WS] Received pong, latency: ${Date.now() - data.t}ms`);
            return;
        }
        
        if (data.type === 'connection_taken_over') {
            console.log(`[WS] Connection taken over by new connection ${data.new_connection_id}`);
            this.disconnect('Connection taken over');
            return;
        }
        
        if (data.type === 'joined') {
            console.log(`[WS] Joined room:`, data.room);
            if (window.onChatJoined) {
                window.onChatJoined(data);
            }
            return;
        }
        
        if (data.type === 'message' || data.type === 'chat_message' || data.type === 'reaction' || data.type === 'message_deleted') {
            console.log(`[WS] Message received:`, data);
            if (window.onChatMessage) {
                window.onChatMessage(data);
            }
            return;
        }
        
        if (data.type === 'error') {
            console.error(`[WS] Server error: ${data.code}`, data);
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
        
        if (data.type === 'ping') {
            console.log('[WS] Received ping from server');
            this.send({
                type: 'pong',
                t: Date.now()
            });
            return;
        }
        
        if (data && typeof data.type === 'string' && data.type.startsWith('dm_call_')) {
            if (window.onDmCallEvent) {
                try { window.onDmCallEvent(data); } catch (e) { console.error('[WS] onDmCallEvent error', e); }
            } else if (window.onWsMessage) {
                try { window.onWsMessage(data); } catch (e) { console.error('[WS] onWsMessage error', e); }
            } else {
                console.log('[WS] DM call event (no handler):', data);
            }
            return;
        }

        if (data && typeof data.type === 'string' && (data.type.startsWith('voice_') || data.type.startsWith('rtc_'))) {
            if (window.onVoiceEvent) {
                try { window.onVoiceEvent(data); } catch (e) { console.error('[WS] onVoiceEvent error', e); }
            } else {
                console.log('[WS] Voice event (no handler):', data);
            }
            return;
        }

        if (window.onWsMessage) {
            try { window.onWsMessage(data); } catch (e) { console.error('[WS] onWsMessage error', e); }
            return;
        }

        console.log('[WS] Unknown message:', data);
    }
    
    _handleClose(event, connectionId) {
        if (connectionId !== this.connectionId) {
            console.log(`[WS ${connectionId}] Ignoring close for stale connection`);
            return;
        }
        
        this.isAuthenticated = false;
        
        if (this.pingInterval) {
            clearInterval(this.pingInterval);
            this.pingInterval = null;
        }

        if (event.code === 1008 || event.code === 4001) {
            console.warn('[WS] Fatal close, stopping reconnect:', event.code, event.reason);
            try { localStorage.removeItem('auth_token'); } catch (_) {}
            try { localStorage.removeItem('refresh_token'); } catch (_) {}
            return;
        }
        
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
            if (token && !_lbIsJwtExpired(token)) {
                this.connect(token).catch(() => {});
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
        if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
            console.log('[WS] Cannot send - WebSocket not connected');
            if (data.type !== 'ping') {
                this.pendingMessages.push(data);
            }
            return false;
        }
        
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
        
        if (this.reconnectTimer) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = null;
        }
        
        if (this.pingInterval) {
            clearInterval(this.pingInterval);
            this.pingInterval = null;
        }
        
        if (this.ws) {
            this.ws.onclose = null;
            if (this.ws.readyState === WebSocket.OPEN) {
                this.ws.close(1000, reason);
            }
            this.ws = null;
        }
    }

    getStatus() {
        return {
            connected: this.ws && this.ws.readyState === WebSocket.OPEN,
            authenticated: this.isAuthenticated,
            pendingMessages: this.pendingMessages.length,
            reconnectAttempts: this.reconnectAttempts
        };
    }

    get isConnected() {
        return !!(this.ws && this.ws.readyState === WebSocket.OPEN);
    }
}

export const wsManager = new WebSocketManager();

window.wsManager = wsManager;