// app.js - В НАЧАЛО ФАЙЛА
console.log('[APP] Module loading started...');

// Проверяем, что все зависимости загружены
if (typeof fetch === 'undefined') {
    console.error('[APP] fetch is not available!');
    alert('Ваш браузер устарел или блокирует JavaScript');
    throw new Error('fetch not available');
}

// Теперь импорты
import { api } from "./api.js";
import { initFriends } from "./friends.js";
import { wsManager } from "./websocket-manager.js";

console.log('[APP] All imports loaded successfully');

const $ = (id) => document.getElementById(id);

// ===== ANTI-RELOAD SYSTEM =====
let isPageUnloading = false;

window.addEventListener('beforeunload', () => {
    isPageUnloading = true;
    console.log('[APP] Page unloading, disconnecting WS');
    if (wsManager.disconnect) wsManager.disconnect('Page unload');
});

// ===== STATE =====
let currentServerId = null;
let currentChatId = null;
let currentUser = null;
let isInitialized = false;
let isOpeningServer = false;

// ===== ОТЛАДОЧНЫЕ ФУНКЦИИ =====
function testServerButton() {
    console.log('[TEST] Testing server button functionality');
    
    // Создаем тестовую кнопку
    const testBtn = document.createElement('button');
    testBtn.textContent = 'Test: Simulate server click';
    testBtn.style.cssText = `
        position: fixed;
        top: 100px;
        right: 10px;
        z-index: 9999;
        padding: 10px;
        background: #00ff00;
        color: black;
        border: none;
        border-radius: 5px;
        cursor: pointer;
        font-size: 12px;
    `;
    
    testBtn.addEventListener('click', () => {
        console.log('[TEST] Simulating server click');
        const serverItem = document.querySelector('.item.server');
        if (serverItem) {
            console.log('[TEST] Found server item, triggering click');
            console.log('[TEST] Server ID:', serverItem.dataset.serverId);
            console.log('[TEST] Current server ID:', currentServerId);
            serverItem.click();
        } else {
            console.error('[TEST] No server item found!');
            console.log('[TEST] Available servers:', document.querySelectorAll('.item.server').length);
        }
    });
    
    document.body.appendChild(testBtn);
}

// ===== UI FUNCTIONS =====
async function loadMe() {
    try {
        console.log("[UI] Loading current user...");
        const me = await api("/api/users/me");
        currentUser = me;
        $("userName").textContent = me.nickname || me.username;
        $("userAvatar").textContent = (me.nickname || me.username)[0].toUpperCase();
        console.log(`[ME] Loaded as ${currentUser.username}`);
    } catch (e) {
        console.error("[ME] Failed to load current user", e);
        
        if (e.status === 401 || e.message.includes('401')) {
            console.error('[ME] Token invalid or expired, redirecting to login');
            localStorage.removeItem('auth_token');
            sessionStorage.clear();
            window.location.href = "/";
            return;
        }
        
        throw e;
    }
}

// ===== МЕНЕДЖМЕНТ МЕНЮ КАНАЛОВ =====
function showChannelsMenu() {
    const channelsPanel = document.querySelector('.panel.channels');
    if (channelsPanel) {
        channelsPanel.classList.add('show-channels');
        console.log('[UI] Channels menu shown');
    }
}

function hideChannelsMenu() {
    const channelsPanel = document.querySelector('.panel.channels');
    if (channelsPanel) {
        channelsPanel.classList.remove('show-channels');
        console.log('[UI] Channels menu hidden');
    }
}

function toggleChannelsMenu() {
    const channelsPanel = document.querySelector('.panel.channels');
    if (channelsPanel) {
        const isVisible = channelsPanel.classList.contains('show-channels');
        console.log('[UI] Toggling channels menu, currently visible:', isVisible);
        if (isVisible) {
            hideChannelsMenu();
        } else {
            showChannelsMenu();
        }
    }
}

// ===== RENDER SERVERS =====
function renderServers(servers) {
    console.log('[DEBUG] renderServers called', {
        serversCount: servers?.length,
        currentServerId,
        sessionServerId: sessionStorage.getItem("lastServerId")
    });
    
    const serversList = document.getElementById('servers-list');
    if (!serversList) {
        console.error('[ERROR] servers-list element not found!');
        return;
    }
    
    // Очищаем список
    serversList.innerHTML = '';
    
    if (!servers || servers.length === 0) {
        serversList.innerHTML = `
            <div class="empty-servers">
                <p>Нет серверов</p>
                <button class="btn btn-ghost" id="addServerBtn">Создать сервер</button>
            </div>
        `;
        return;
    }
    
    servers.forEach(server => {
        const serverItem = document.createElement('div');
        const isActive = server.id === currentServerId;
        serverItem.className = `item server ${isActive ? 'active' : ''}`;
        serverItem.dataset.serverId = server.id;
        serverItem.dataset.testId = `server-${server.id}`;
        
        serverItem.innerHTML = `
            <div class="avatar">${server.name[0]?.toUpperCase() || 'S'}</div>
            <div class="text">
                <div class="title">${server.name}</div>
                <div class="sub">${server.description || 'Сервер'}</div>
            </div>
        `;
        
        // Простой обработчик клика
        serverItem.addEventListener('click', (e) => {
            e.stopPropagation();
            e.preventDefault();
            
            console.log('[CLICK] Server clicked:', {
                id: server.id,
                name: server.name,
                currentServerId,
                isActive
            });
            
            // Если сервер уже активен
            if (currentServerId === server.id) {
                console.log(`[UI] Server ${server.id} already active, refreshing UI`);
                
                // Визуальная обратная связь
                serverItem.classList.add('refreshing');
                setTimeout(() => serverItem.classList.remove('refreshing'), 300);
                
                // На мобильных устройствах переключаем меню каналов
                if (window.innerWidth <= 900) {
                    toggleChannelsMenu();
                }
                
                return;
            }
            
            console.log(`[UI] Opening server ${server.id} (${server.name})`);
            openServer(server.id, server.name);
        });
        
        serversList.appendChild(serverItem);
    });
    
    console.log('[DEBUG] Servers rendered:', serversList.children.length);
}

// ===== OPEN SERVER =====
async function openServer(serverId, serverName) {
    if (isOpeningServer) {
        console.log('[UI] Server opening in progress, skipping');
        return;
    }
    
    isOpeningServer = true;
    
    try {
        console.log(`[UI] Opening server ${serverId} (${serverName})`);
        
        // Обновляем состояние
        currentServerId = serverId;
        sessionStorage.setItem("lastServerId", serverId.toString());
        
        console.log('[DEBUG] State updated:', {
            currentServerId,
            sessionStorage: sessionStorage.getItem("lastServerId")
        });
        
        // Обновляем активный сервер в UI
        document.querySelectorAll('.item.server').forEach(item => {
            const itemId = parseInt(item.dataset.serverId);
            item.classList.toggle('active', itemId === serverId);
        });
        
        // Загружаем чаты сервера
        const chats = await api(`/api/servers/${serverId}/chats`);
        console.log(`[UI] Loaded ${chats.length} chats for server ${serverId}`);
        
        renderChannels(chats);
        
        // Показываем меню каналов на мобильных устройствах
        if (window.innerWidth <= 900) {
            showChannelsMenu();
        }
        
        // Открываем первый канал или сохраненный
        const lastChatId = Number(sessionStorage.getItem("lastChatId"));
        const chatId = chats.find(c => c.id === lastChatId)?.id ?? chats[0]?.id;
        
        if (chatId) {
            const chat = chats.find(c => c.id === chatId);
            await openChat(chatId, chat?.name || 'Unknown');
        } else if (chats.length > 0) {
            await openChat(chats[0].id, chats[0].name);
        }
        
    } catch (error) {
        console.error("[UI] Failed to load server chats", error);
    } finally {
        isOpeningServer = false;
    }
}

// ===== RENDER CHANNELS =====
function renderChannels(chats) {
    const channelsList = document.getElementById('channels-list');
    if (!channelsList) {
        console.error('[ERROR] channels-list element not found!');
        return;
    }
    
    channelsList.innerHTML = '';
    
    if (!chats || chats.length === 0) {
        channelsList.innerHTML = `
            <div class="empty-channels">
                <p>Нет каналов</p>
            </div>
        `;
        return;
    }
    
    chats.forEach(chat => {
        const channelItem = document.createElement('div');
        const isActive = chat.id === currentChatId;
        channelItem.className = `item channel ${isActive ? 'active' : ''}`;
        channelItem.dataset.channelId = chat.id;
        
        channelItem.innerHTML = `
            <span class="hash">#</span>
            <div class="text">
                <div class="title">${chat.name}</div>
                <div class="sub">${chat.description || 'Канал'}</div>
            </div>
        `;
        
        channelItem.addEventListener('click', () => {
            openChat(chat.id, chat.name);
        });
        
        channelsList.appendChild(channelItem);
    });
}

// ===== CHAT FUNCTIONS =====
async function openChat(chatId, title) {
    console.log(`[UI] Opening chat ${chatId} (${title})`);
    
    // Обновляем состояние
    currentChatId = chatId;
    sessionStorage.setItem("lastChatId", chatId.toString());
    
    // Обновляем заголовок чата
    const chatTitleElement = $("chat-title");
    if (chatTitleElement) {
        chatTitleElement.textContent = `# ${title}`;
    }
    
    // Обновляем активный канал
    document.querySelectorAll('.item.channel').forEach(item => {
        const itemId = parseInt(item.dataset.channelId);
        item.classList.toggle('active', itemId === chatId);
    });
    
    // Скрываем меню каналов на мобильных устройствах
    if (window.innerWidth <= 900) {
        hideChannelsMenu();
    }
    
    // Очищаем сообщения
    const messagesContainer = $("messages");
    if (messagesContainer) {
        messagesContainer.innerHTML = '';
    }
    
    try {
        const msgs = await api(`/api/servers/${currentServerId}/chats/${chatId}/messages`);
        
        if (Array.isArray(msgs) && msgs.length > 0) {
            msgs.forEach(addMessage);
        } else {
            // Показываем сообщение о пустом чате
            const emptyMsg = document.createElement('div');
            emptyMsg.className = 'empty-chat';
            emptyMsg.innerHTML = `
                <h3>Добро пожаловать в #${title}! 👋</h3>
                <p>Это начало канала #${title}. Напишите первое сообщение!</p>
            `;
            if (messagesContainer) {
                messagesContainer.appendChild(emptyMsg);
            }
        }
        
        // Присоединяемся к комнате WebSocket
        if (wsManager && wsManager.isConnected && wsManager.joinRoom) {
            wsManager.joinRoom(chatId);
            console.log(`[WS] Joined room ${chatId}`);
        } else {
            console.warn(`[WS] Cannot join room ${chatId} - WebSocket not connected`);
        }
    } catch (e) {
        console.error("[UI] Failed to load messages", e);
    }
}

// ===== WEB SOCKET INTEGRATION =====
function setupWebSocketHandlers() {
    // Глобальный обработчик сообщений чата
    window.onChatMessage = (data) => {
        console.log('[APP] WebSocket message received:', data);
        if (data.room_id === currentChatId) {
            addMessage({
                sender_username: data.sender_username || data.sender_id,
                content: data.content
            });
        }
    };
}

function initWebSocket() {
    const token = localStorage.getItem('auth_token');
    if (!token) {
        console.warn('No token available for WebSocket');
        return;
    }
    
    setTimeout(() => {
        if (!isPageUnloading && wsManager.connect) {
            console.log('[APP] Attempting WebSocket connection...');
            wsManager.connect(token).then(() => {
                console.log('[APP] WebSocket connected successfully');
                
                // Присоединяемся к текущему чату если есть
                if (currentChatId && wsManager.isConnected && wsManager.joinRoom) {
                    console.log(`[WS] Rejoining room ${currentChatId} after reconnect`);
                    wsManager.joinRoom(currentChatId);
                }
            }).catch(error => {
                console.error('[APP] WebSocket connection failed:', error);
            });
        }
    }, 500);
}

// ===== MESSAGE SENDING =====
function setupMessageComposer() {
    const composerForm = document.getElementById('composer');
    if (!composerForm) {
        console.error('[APP] Composer form not found!');
        return;
    }
    
    // Удаляем старый обработчик
    const newForm = composerForm.cloneNode(true);
    composerForm.parentNode.replaceChild(newForm, composerForm);
    
    const form = document.getElementById('composer');
    const input = document.getElementById('message');
    let isSubmitting = false;
    
    form.addEventListener('submit', async (e) => {
        e.preventDefault();
        e.stopImmediatePropagation();
        
        if (isSubmitting) return;
        isSubmitting = true;
        
        const message = input.value.trim();
        if (!message) {
            isSubmitting = false;
            return;
        }
        
        if (!currentChatId) {
            alert('Ошибка: не выбран чат для отправки');
            isSubmitting = false;
            return;
        }
        
        console.log('[APP] Sending message:', message);
        const originalMessage = message;
        input.value = '';
        
        try {
            // Убираем сообщение о пустом чате
            const emptyMsg = $("messages").querySelector('.empty-chat');
            if (emptyMsg) emptyMsg.remove();
            
            // Оптимистичное обновление
            addMessage({
                sender_username: currentUser?.username || 'Вы',
                content: originalMessage
            });
            
            // Отправляем через HTTP API
            const token = localStorage.getItem('auth_token');
            const response = await fetch(`/api/servers/${currentServerId}/chats/${currentChatId}/messages`, {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    'Authorization': `Bearer ${token}`
                },
                body: JSON.stringify({ content: originalMessage })
            });
            
            if (!response.ok) {
                throw new Error(`HTTP ${response.status}: ${await response.text()}`);
            }
            
            console.log('[APP] Message sent successfully');
            
        } catch (error) {
            console.error('[APP] Failed to send message:', error);

            if (error.message.includes('401') || error.message.includes('Unauthorized')) {
                console.error('[APP] Token invalid, clearing and redirecting to login');
                localStorage.removeItem('auth_token');
                sessionStorage.clear();
                window.location.href = "/";
                return;
            }

            input.value = originalMessage;
            
            const errorDiv = document.createElement('div');
            errorDiv.textContent = `Не удалось отправить сообщение. Попробуйте еще раз.`;
            errorDiv.style.cssText = `
                background: rgba(255, 50, 50, 0.1);
                color: #ff5555;
                padding: 10px;
                margin: 10px;
                border-radius: 5px;
                border: 1px solid #ff5555;
                font-size: 14px;
            `;
            
            $("messages").appendChild(errorDiv);
            setTimeout(() => errorDiv.remove(), 3000);
        } finally {
            isSubmitting = false;
        }
    });
    
    input.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault();
            form.dispatchEvent(new Event('submit'));
        }
    });
    
    console.log('[APP] Message composer setup complete');
}

// ===== addMessage =====
function addMessage(msg) {
    const div = document.createElement("div");
    div.className = "message";
    
    const isCurrentUser = msg.sender_username === (currentUser?.username || 'Вы');
    
    div.innerHTML = `
        <div class="avatar ${isCurrentUser ? 'you' : ''}">
            ${(msg.sender_username || '?')[0].toUpperCase()}
        </div>
        <div class="content">
            <div class="author">
                ${msg.sender_username} 
                ${isCurrentUser ? '<span class="badge">Вы</span>' : ''}
            </div>
            <div class="text">${msg.content}</div>
            <div class="time">${new Date().toLocaleTimeString([], {hour: '2-digit', minute:'2-digit'})}</div>
        </div>
    `;
    
    $("messages").appendChild(div);
    
    setTimeout(() => {
        $("messages").scrollTop = $("messages").scrollHeight;
    }, 100);
}

// ===== INIT =====
async function initializeApp() {
    if (isInitialized) {
        console.warn('[APP] Already initialized, skipping');
        return;
    }
    
    console.log('[APP] Initializing...');
    isInitialized = true;
    
    const overlay = $("loading-overlay");
    if (overlay) overlay.classList.remove("hidden");

    try {
        // Загружаем пользователя
        await loadMe();
        
        // Инициализируем друзей
        await initFriends();
        
        // Настраиваем WebSocket обработчики
        setupWebSocketHandlers();
        
        // Настраиваем композер сообщений
        setupMessageComposer();
        
        // Загружаем серверы
        const servers = await api("/api/servers");
        console.log('[APP] Servers loaded:', servers);
        
        // ВОССТАНАВЛИВАЕМ СОСТОЯНИЕ ИЗ SESSIONSTORAGE
        const lastServerId = Number(sessionStorage.getItem("lastServerId"));
        const lastChatId = Number(sessionStorage.getItem("lastChatId"));
        
        console.log('[APP] Restoring from sessionStorage:', {
            lastServerId,
            lastChatId,
            serversCount: servers.length
        });
        
        // Выбираем сервер для открытия
        let serverId = lastServerId;
        if (!serverId || !servers.find(s => s.id === serverId)) {
            serverId = servers[0]?.id;
        }
        
        // Рендерим серверы (текущий будет отмечен как активный)
        if (serverId) {
            currentServerId = serverId;
        }
        renderServers(servers);
        
        // Загружаем чаты выбранного сервера
        if (serverId) {
            const chats = await api(`/api/servers/${serverId}/chats`);
            console.log('[APP] Chats loaded:', chats);
            
            renderChannels(chats);
            
            // Выбираем чат для открытия
            let chatId = lastChatId;
            if (!chatId || !chats.find(c => c.id === chatId)) {
                chatId = chats[0]?.id;
            }
            
            if (chatId) {
                const chat = chats.find(c => c.id === chatId);
                await openChat(chatId, chat?.name || 'Unknown');
            }
        }
        
        // Инициализируем WebSocket
        setTimeout(() => {
            initWebSocket();
        }, 500);
        
        // Добавляем тестовую кнопку
        testServerButton();
        
    } catch (error) {
        console.error('[APP] Initialization failed:', error);
        
        if (error.status === 401 || error.message.includes('401') || error.message.includes('Unauthorized')) {
            console.error('[APP] Auth error, clearing token and reloading');
            localStorage.removeItem('auth_token');
            sessionStorage.clear();
            window.location.href = "/";
            return;
        }
        
        alert(`Ошибка инициализации: ${error.message}. Перезагрузите страницу.`);
    } finally {
        if (overlay) {
            setTimeout(() => {
                overlay.classList.add("hidden");
            }, 300);
        }
    }
}

// ===== EVENT LISTENERS =====
document.addEventListener("DOMContentLoaded", () => {
    console.log('[APP] DOM loaded, checking auth...');
    
    const token = localStorage.getItem("auth_token");
    if (!token) {
        console.log('[APP] No auth token, redirecting to login...');
        window.location.href = "/";
        return;
    }
    
    console.log('[APP] Auth token found, initializing...');
    initializeApp();
});

// ===== ОБРАБОТЧИКИ ДЛЯ АДАПТИВНОСТИ =====
window.addEventListener('resize', () => {
    if (window.innerWidth > 900) {
        hideChannelsMenu();
    }
});

// ===== ГЛОБАЛЬНЫЕ ОБРАБОТЧИКИ ОШИБОК =====
window.addEventListener('error', (event) => {
    console.error('[GLOBAL ERROR]', event.error);
});

window.addEventListener('unhandledrejection', (event) => {
    console.error('[UNHANDLED PROMISE REJECTION]', event.reason);
});

// Экспорт для отладки
window.appState = {
    currentServerId,
    currentChatId,
    currentUser,
    wsManager,
    reload: () => {
        sessionStorage.setItem("lastChatId", currentChatId);
        sessionStorage.setItem("lastServerId", currentServerId);
        window.location.reload();
    }
};

window.wsManager = wsManager;

if (window.appInitialized) {
    console.warn('[APP] App already initialized elsewhere');
} else {
    window.appInitialized = true;
}

console.log('[APP] Application script loaded successfully');