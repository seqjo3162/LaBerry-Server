console.log('[APP] Module loading started...');

if (typeof fetch === 'undefined') {
    console.error('[APP] fetch is not available!');
    alert('Ваш браузер устарел или блокирует JavaScript');
    throw new Error('fetch not available');
}

import { api } from "./api.js?v=9";
import { initFriends } from "./friends.js?v=8";
import { wsManager } from "./websocket-manager.js?v=11";
import { createSettingsUI } from "./settings.js?v=9";
import { showUserMenu } from "./user-menu.js?v=7";
import { initVoice } from "./voice.js?v=13";
import { initProfileModal } from "./profile-modal.js?v=8";

console.log('[APP] All imports loaded successfully');

window.lbShowUserMenu = showUserMenu;

const $ = (id) => document.getElementById(id);

// ui scale: fast paint from localStorage
try {
    const s = localStorage.getItem('ui_scale');
    if (s) {
        document.documentElement.style.setProperty('--ui-scale', String(s));
        const scaleNum = Number(s);
        const scaled = Number.isFinite(scaleNum) && Math.abs(scaleNum - 1) > 0.001;
        document.documentElement.classList.toggle('ui-scaled', scaled);
        document.body?.classList?.toggle?.('ui-scaled', scaled);
        document.getElementById('appRoot')?.classList?.toggle?.('ui-scaled-root', scaled);
    }
} catch (_) {}

const chatNameById = new Map();
const chatKindById = new Map();
const serverOwnerById = new Map(); // server_id -> owner_id (for channel management)
let lastVoiceSwitchClick = { id: null, at: 0 };

let settingsSnapshot = null;

function avatarRawUrl(fileId) {
    const id = Number(fileId);
    if (!Number.isFinite(id) || id <= 0) return null;
    return `/api/profile-files/${id}/raw`;
}

function avatarInnerHtml(fileId, usernameFallback) {
    const url = avatarRawUrl(fileId);
    if (url) {
        const alt = escapeHtml(usernameFallback || '');
        return `<img class="avatar-img" src="${url}" alt="${alt}">`;
    }
    const letter = String(usernameFallback || '?').trim().charAt(0).toUpperCase() || '?';
    return escapeHtml(letter);
}

function applyUserbarAvatar() {
    const img = $('userAvatarImg');
    const txt = $('avatarText');

    const username = currentUser?.username || 'U';
    const letter = String(username).trim().charAt(0).toUpperCase() || 'U';
    if (txt) txt.textContent = letter;

    const fid = currentUserProfile?.avatar_file_id;
    const url = avatarRawUrl(fid);

    if (img) {
        if (url) {
            img.src = url + `?t=${Date.now()}`;
            img.style.display = '';
            if (txt) txt.style.display = 'none';
        } else {
            img.removeAttribute('src');
            img.style.display = 'none';
            if (txt) txt.style.display = '';
        }
    }
}

window.addEventListener('laberry:avatar-updated', (ev) => {
    const fid = ev?.detail?.avatar_file_id;
    const id = Number(fid);
    if (!Number.isFinite(id) || id <= 0) return;

    if (!currentUserProfile) currentUserProfile = {};
    currentUserProfile.avatar_file_id = id;

    applyUserbarAvatar();
});

let audioCtx = null;
let lastDesktopAt = 0;
let lastSoundAt = 0;

// reply draft
let replyToMessageId = null;
let replyToPreview = null; // { sender, text }
let replyBarEl = null;

// emoji picker
let emojiPickerEl = null;
let emojiPickerBackdrop = null;


let isPageUnloading = false;

window.addEventListener('beforeunload', () => {
    isPageUnloading = true;
    console.log('[APP] Page unloading, disconnecting WS');
    if (wsManager.disconnect) wsManager.disconnect('Page unload');
});

let currentServerId = null;
let currentChatId = null;

// last opened NON-voice text channel per server (for returning after leaving voice)
const lastTextChatByServer = new Map(); // serverId -> { id:number, name:string }
let currentUser = null;
let currentUserProfile = null;
let isInitialized = false;
let isOpeningServer = false;
let openChatSeq = 0;
let membersPollTimer = null;

// ===== CHAT SCROLL + UNREAD (client-side) =====
const SCROLL_BOTTOM_THRESHOLD_PX = 80;

let messagesAutoWired = false;
let stickToBottomEnabled = false;
let stickToBottomChatId = null;
let lastScrollUpdateAt = 0;

function lsKeyLastSeen(serverId, chatId) {
    return `lb:lastSeen:${serverId || 0}:${chatId || 0}`;
}
function lsKeyUnread(serverId, chatId) {
    return `lb:unread:${serverId || 0}:${chatId || 0}`;
}
function getLastSeenId(serverId, chatId) {
    try {
        const v = localStorage.getItem(lsKeyLastSeen(serverId, chatId));
        const n = v ? Number(v) : null;
        return Number.isFinite(n) ? n : null;
    } catch (_) { return null; }
}
function setLastSeenId(serverId, chatId, id) {
    if (id === null || id === undefined) return;
    const n = Number(id);
    if (!Number.isFinite(n)) return;
    try { localStorage.setItem(lsKeyLastSeen(serverId, chatId), String(n)); } catch (_) {}
}
function getUnreadCount(serverId, chatId) {
    try {
        const v = localStorage.getItem(lsKeyUnread(serverId, chatId));
        const n = v ? Number(v) : 0;
        return Number.isFinite(n) && n > 0 ? n : 0;
    } catch (_) { return 0; }
}
function setUnreadCount(serverId, chatId, count) {
    const n = Number(count);
    try {
        if (!Number.isFinite(n) || n <= 0) localStorage.removeItem(lsKeyUnread(serverId, chatId));
        else localStorage.setItem(lsKeyUnread(serverId, chatId), String(Math.floor(n)));
    } catch (_) {}
}
function incUnreadCount(serverId, chatId, by = 1) {
    const cur = getUnreadCount(serverId, chatId);
    setUnreadCount(serverId, chatId, cur + (Number(by) || 1));
}
function clearUnreadCount(serverId, chatId) {
    setUnreadCount(serverId, chatId, 0);
}

function isAtBottomEl(container, threshold = SCROLL_BOTTOM_THRESHOLD_PX) {
    if (!container) return true;
    const dist = container.scrollHeight - container.scrollTop - container.clientHeight;
    return dist <= threshold;
}

function getLatestRenderedMessageId(container) {
    if (!container) return null;
    const last = container.querySelector?.('.message[data-msg-id]:last-of-type');
    if (!last) {
        // fallback: last message in DOM
        const all = container.querySelectorAll?.('.message[data-msg-id]');
        const el = all && all.length ? all[all.length - 1] : null;
        const v = el?.dataset?.msgId;
        const n = v ? Number(v) : null;
        return Number.isFinite(n) ? n : null;
    }
    const v = last.dataset.msgId;
    const n = v ? Number(v) : null;
    return Number.isFinite(n) ? n : null;
}

function scrollToBottomNow(container) {
    if (!container) return;
    container.scrollTop = container.scrollHeight;
}

function scrollToBottomSafe(container, frames = 3) {
    if (!container) return;
    let left = Math.max(1, Math.min(10, Number(frames) || 3));
    const tick = () => {
        if (!container) return;
        scrollToBottomNow(container);
        left -= 1;
        if (left > 0) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
}

function ensureNewMarker(container) {
    if (!container) return null;
    let m = container.querySelector('#newMessagesMarker');
    if (!m) {
        m = document.createElement('div');
        m.id = 'newMessagesMarker';
        m.className = 'new-messages-marker';
        m.textContent = 'Новые сообщения';
    }
    return m;
}

function removeNewMarker(container) {
    if (!container) return;
    const m = container.querySelector('#newMessagesMarker');
    if (m) m.remove();
}

function insertNewMarkerAfter(container, lastSeenId) {
    if (!container) return;
    removeNewMarker(container);

    const id = Number(lastSeenId);
    if (!Number.isFinite(id)) return;

    const anchor = container.querySelector(`.message[data-msg-id="${id}"]`);
    if (!anchor) return;

    const marker = ensureNewMarker(container);
    if (!marker) return;

    if (anchor.nextSibling) container.insertBefore(marker, anchor.nextSibling);
    else container.appendChild(marker);
}

function scrollToMessageId(container, id) {
    if (!container) return false;
    const n = Number(id);
    if (!Number.isFinite(n)) return false;
    const el = container.querySelector(`.message[data-msg-id="${n}"]`);
    if (!el) return false;

    // align near top, with some padding
    el.scrollIntoView({ block: 'start' });
    container.scrollTop = Math.max(0, container.scrollTop - 24);
    return true;
}

function ensureJumpToPresentBtn() {
    const chatView = document.getElementById('chatView');
    if (!chatView) return null;

    let btn = document.getElementById('jumpToPresentBtn');
    if (!btn) {
        btn = document.createElement('button');
        btn.id = 'jumpToPresentBtn';
        btn.type = 'button';
        btn.className = 'jump-to-present hidden';
        btn.textContent = 'Новые сообщения';
        chatView.appendChild(btn);

        btn.addEventListener('click', () => {
            const c = document.getElementById('messages');
            if (!c) return;
            removeNewMarker(c);
            clearUnreadCount(currentServerId, currentChatId);
            setStickToBottom(true);
            scrollToBottomSafe(c, 4);
            updateJumpBtn();
        });
    }
    return btn;
}

function setStickToBottom(enabled) {
    stickToBottomEnabled = !!enabled;
    stickToBottomChatId = enabled ? currentChatId : null;
}

function updateJumpBtn() {
    const btn = document.getElementById('jumpToPresentBtn');
    if (!btn) return;

    const c = document.getElementById('messages');
    const unread = currentServerId && currentChatId ? getUnreadCount(currentServerId, currentChatId) : 0;
    const show = !!c && unread > 0 && !isAtBottomEl(c, SCROLL_BOTTOM_THRESHOLD_PX);

    btn.classList.toggle('hidden', !show);
    if (show) {
        btn.textContent = unread > 99 ? '99+ новых' : `${unread} новых`;
    }
}

function wireMessagesAutoScroll() {
    if (messagesAutoWired) return;
    const container = document.getElementById('messages');
    if (!container) return;

    messagesAutoWired = true;

    const tryStick = () => {
        if (!container) return;
        if (!stickToBottomEnabled) return;
        if (stickToBottomChatId !== currentChatId) return;
        scrollToBottomSafe(container, 2);
    };

    // when media loads, height changes -> keep bottom if needed
    const onMediaLoad = () => {
        // avoid spam in case of many images
        const now = Date.now();
        if (now - lastScrollUpdateAt < 20) return;
        lastScrollUpdateAt = now;

        tryStick();
    };

    container.addEventListener('load', onMediaLoad, true); // capture: load doesn't bubble
    container.addEventListener('loadedmetadata', onMediaLoad, true);
    container.addEventListener('canplay', onMediaLoad, true);
    container.addEventListener('error', onMediaLoad, true);

    // also react to size changes (new DOM / image decode, etc.)
    try {
        const ro = new ResizeObserver(() => onMediaLoad());
        ro.observe(container);
    } catch (_) {}

    container.addEventListener('scroll', () => {
        if (!container) return;

        // user scrolled up -> stop sticking
        if (isAtBottomEl(container, SCROLL_BOTTOM_THRESHOLD_PX)) {
            setStickToBottom(true);

            // mark last seen, but do it sparingly
            const now = Date.now();
            if (now - lastScrollUpdateAt > 200) {
                lastScrollUpdateAt = now;
                const lastId = getLatestRenderedMessageId(container);
                if (lastId !== null && currentServerId && currentChatId) {
                    setLastSeenId(currentServerId, currentChatId, lastId);
                    clearUnreadCount(currentServerId, currentChatId);
                    removeNewMarker(container);
                }
            }
        } else {
            setStickToBottom(false);
        }

        updateJumpBtn();
    }, { passive: true });
}


// messages pagination (последние 50 + подгрузка вверх)
const MESSAGES_PAGE_SIZE = 50;
let chatPaging = { chatId: null, minId: null, hasMore: true, loading: false };

let attachmentUiReady = false;
let attachmentViewer = null;

const seenMessageKeys = new Set();
function msgKey(chatId, id) {
    return `${chatId}:${id}`;
}
function hasSeen(chatId, id) {
    if (id === null || id === undefined) return false;
    return seenMessageKeys.has(msgKey(chatId, id));
}
function markSeen(chatId, id) {
    if (id === null || id === undefined) return;
    seenMessageKeys.add(msgKey(chatId, id));
}

function normalizeHash() {
    // поддержка старого формата (#friends)
    if (location.hash === '#friends') {
        try { history.replaceState(null, '', `${location.pathname}${location.search}#/friends`); }
        catch (_) { location.hash = '#/friends'; }
    }
}

function statusToClass(status) {
    const s = (status || 'online').toString().toLowerCase();
    if (s === 'idle' || s === 'dnd' || s === 'offline' || s === 'invisible') return s;
    return 'online';
}

function statusToLabel(status) {
    const cls = statusToClass(status);
    if (cls === 'idle') return 'Нет на месте';
    if (cls === 'dnd') return 'Не беспокоить';
    if (cls === 'offline' || cls === 'invisible') return 'Не в сети';
    return 'В сети';
}

function applyMyStatusToUI(status) {
    const cls = statusToClass(status);
    const top = document.getElementById('status');
    const mini = document.getElementById('userStatus');

    const text = statusToLabel(cls);

    if (top) {
        top.textContent = text;
        top.className = `muted status ${cls === 'invisible' ? 'offline' : cls}`;
    }
    if (mini) {
        mini.textContent = text;
        mini.className = `status ${cls === 'invisible' ? 'offline' : cls}`;
    }
}

async function loadMyStatus() {
    try {
        const st = await api('/api/users/me/status');
        applyMyStatusToUI(st?.status || 'online');
        return st?.status || 'online';
    } catch (_) {
        applyMyStatusToUI('online');
        return 'online';
    }
}

async function updateMyStatus(status) {
    const next = statusToClass(status);
    applyMyStatusToUI(next);

    try {
        await api('/api/users/me/status', {
            method: 'PUT',
            body: JSON.stringify({ status: next })
        });
    } catch (e) {
        console.warn('[SETTINGS] Failed to update status', e);
    }

    // быстрый рефреш, чтобы статус не "лагал" в списках
    try { refreshFriendsStatus(); } catch (_) {}
    if (currentServerId) {
        try { await loadMembers(currentServerId); } catch (_) {}
    }
}



function getSettings() {
    if (settingsSnapshot) return settingsSnapshot;
    if (settingsUI && settingsUI.getSettings) return settingsUI.getSettings();
    return null;
}

function canNotifyNow() {
    const s = getSettings();
    if (!s) return { desktop: false, sound: false };

    const desktop = !!s.notify_desktop && typeof Notification !== 'undefined' && Notification.permission === 'granted';
    const sound = !!s.notify_sounds;
    return { desktop, sound };
}

function isUserWatchingChat(chatId) {
    // friends view hides chat
    if (location.hash === '#/friends' || location.hash === '#friends') return false;

    const chatView = document.getElementById('chatView');
    if (!chatView || chatView.classList.contains('hidden')) return false;

    if (chatId !== currentChatId) return false;

    // tab not active
    if (document.hidden || !document.hasFocus()) return false;

    const container = document.getElementById('messages');
    if (!container) return false;

    const dist = container.scrollHeight - container.scrollTop - container.clientHeight;
    const atBottom = dist < 60;
    return atBottom;
}

function playNotifySound() {
    const { sound } = canNotifyNow();
    if (!sound) return;

    // rate limit
    const now = Date.now();
    if (now - lastSoundAt < 700) return;

    try {
        if (!audioCtx) {
            audioCtx = new (window.AudioContext || window.webkitAudioContext)();
        }

        const ctx = audioCtx;
        const o = ctx.createOscillator();
        const g = ctx.createGain();

        o.type = 'sine';
        o.frequency.value = 660;

        g.gain.value = 0.0001;
        o.connect(g);
        g.connect(ctx.destination);

        const t = ctx.currentTime;
        g.gain.setValueAtTime(0.0001, t);
        g.gain.exponentialRampToValueAtTime(0.08, t + 0.01);
        g.gain.exponentialRampToValueAtTime(0.0001, t + 0.18);

        o.start(t);
        o.stop(t + 0.2);

        lastSoundAt = now;
    } catch (_) {
        // ignored (autoplay restrictions)
    }
}

function showDesktopNotification(title, body, tag) {
    const { desktop } = canNotifyNow();
    if (!desktop) return;

    const now = Date.now();
    if (now - lastDesktopAt < 700) return;

    try {
        const n = new Notification(title, {
            body,
            tag: tag ? String(tag) : undefined,
            silent: true, // sound handled separately
        });

        n.onclick = () => {
            try { window.focus(); } catch (_) {}
        };

        lastDesktopAt = now;
    } catch (_) {}
}

function notifyForIncomingMessage(roomId, sender, content) {
    const chatName = chatNameById.get(roomId) || `Чат #${roomId}`;
    const title = `${sender} • ${chatName}`;
    const body = previewTextFromMessageContent((content || '').toString()).slice(0, 120);

    try { maybeUnhideDmOnIncoming(roomId); } catch (_) {}

    // notify only if user is not watching that chat
    if (isUserWatchingChat(roomId)) return;

    // client-side unread counter (for Discord-like open behavior)
    if (currentServerId && roomId !== null && roomId !== undefined) {
        incUnreadCount(currentServerId, roomId, 1);
        updateJumpBtn();
    }

    // desktop notification + sound use same trigger
    showDesktopNotification(title, body, `chat:${roomId}`);
    playNotifySound();
}

let settingsUI = null;

function applyTheme(theme) {
    const t = (theme || 'dark').toString().toLowerCase();
    const root = document.documentElement;
    root.classList.remove('theme-dark', 'theme-light');
    root.classList.add(t === 'light' ? 'theme-light' : 'theme-dark');
    localStorage.setItem('theme', t === 'light' ? 'light' : 'dark');
}

async function openSettings() {
    try { hideChannelsMenu(); } catch (_) {}
    if (settingsUI) {
        settingsUI.open();
    }
}


// ===== Pins (modal) =====
let pinsOverlayEl = null;

function ensurePinsOverlay() {
    if (pinsOverlayEl) return pinsOverlayEl;

    const ov = document.createElement('div');
    ov.className = 'pins-overlay';
    ov.hidden = true;

    ov.innerHTML = `
      <div class="pins-modal" role="dialog" aria-modal="true">
        <div class="pins-top">
          <div style="font-weight:600;">Закрепы</div>
          <button class="icon-btn" type="button" id="pinsCloseBtn" title="Закрыть">✕</button>
        </div>
        <div class="pins-body" id="pinsBody"></div>
      </div>
    `;

    ov.addEventListener('click', (e) => {
        if (e.target === ov) closePinsModal();
    });

    ov.querySelector('#pinsCloseBtn')?.addEventListener('click', closePinsModal);

    document.body.appendChild(ov);
    pinsOverlayEl = ov;
    return ov;
}

function closePinsModal() {
    if (!pinsOverlayEl) return;
    pinsOverlayEl.hidden = true;
}

function pinPreviewText(pin) {
    const raw = (pin?.content || '').toString();
    if (!raw.trim()) {
        return pin?.message_exists === false
            ? 'Сообщение было удалено'
            : 'Не удалось загрузить сообщение';
    }

    const fileNames = extractAllFileNamesFromMessageContent(raw);
    const cleanedText = raw
        .replace(/\[\[file[:=]\d+\|[^\]]*\]\]/g, ' ')
        .replace(/\[\[file:(\d+)\]\][^\]]*\]\]/g, ' ')
        .replace(/\s+/g, ' ')
        .trim();

    const attachmentLines = fileNames.map((name) => `📎 ${name}`);
    if (cleanedText && attachmentLines.length) return `${cleanedText}\n${attachmentLines.join('\n')}`;
    if (attachmentLines.length) return attachmentLines.join('\n');
    return previewTextFromMessageContent(raw) || 'Пустое сообщение';
}

function renderPinRow(pin) {
    const mid = Number(pin?.message_id);
    const pinnedBy = (pin?.pinned_by_username || String(pin?.pinned_by || '')).toString();
    const pinnedAt = formatPinTimestamp(pin?.pinned_at);
    const sender = (pin?.sender_username || '').toString().trim() || 'Неизвестный пользователь';
    const avatarFileId = Number(pin?.sender_avatar_file_id);
    const preview = pinPreviewText(pin);
    const missing = !pin?.content && pin?.message_exists === false;

    return `
      <div class="pin-row" data-mid="${mid}">
        <div class="pin-message">
          <div class="pin-avatar">${avatarInnerHtml(Number.isFinite(avatarFileId) && avatarFileId > 0 ? avatarFileId : null, sender)}</div>
          <div class="pin-main">
            <div class="pin-head">
              <span class="pin-author">${escapeHtml(sender)}</span>
              <span class="pin-dot">•</span>
              <span class="pin-time">${escapeHtml(pinnedAt)}</span>
            </div>
            <div class="pin-submeta">Закрепил ${escapeHtml(pinnedBy)}</div>
            <div class="pin-text${missing ? ' is-missing' : ''}">${escapeHtml(preview)}</div>
            <div class="pin-actions">
              <button class="btn btn-secondary btn-small" type="button" data-act="jump">Перейти</button>
              <button class="btn btn-ghost btn-small" type="button" data-act="unpin">Открепить</button>
            </div>
          </div>
        </div>
      </div>
    `;
}

async function jumpToMessage(messageId) {
    const rid = Number(messageId);
    if (!Number.isFinite(rid) || rid <= 0) return;
    const container = document.getElementById('messages');
    if (!container) return;

    const flash = (el) => {
        try {
            el.scrollIntoView({ block: 'center' });
            el.classList.add('flash');
            setTimeout(() => { try { el.classList.remove('flash'); } catch (_) {} }, 800);
        } catch (_) {}
    };

    let anchor = container.querySelector(`.message[data-msg-id="${rid}"]`);
    if (anchor) {
        flash(anchor);
        return;
    }

    let tries = 0;
    while (!anchor && chatPaging && chatPaging.hasMore && Number(chatPaging.minId || 0) > rid && tries < 20) {
        tries += 1;
        await loadOlderMessages();
        anchor = container.querySelector(`.message[data-msg-id="${rid}"]`);
    }

    if (anchor) {
        flash(anchor);
    } else {
        showToast('Сообщение не найдено');
    }
}

async function openPinsModal() {
    if (!currentChatId) return;
    const chatId = Number(currentChatId);
    if (!Number.isFinite(chatId) || chatId <= 0) return;

    const ov = ensurePinsOverlay();
    const body = ov.querySelector('#pinsBody');
    if (!body) return;

    ov.hidden = false;
    body.innerHTML = '<div class="muted">Загрузка…</div>';

    try {
        const pins = await api(`/api/chats/${chatId}/pins`, { method: 'GET' });
        const items = Array.isArray(pins) ? pins : [];

        if (!items.length) {
            body.innerHTML = '<div class="muted">Нет закрепов</div>';
            return;
        }

        body.innerHTML = items.map(renderPinRow).join('');

        body.querySelectorAll('[data-act="jump"]').forEach((btn) => {
            btn.addEventListener('click', (e) => {
                const row = e.target?.closest?.('[data-mid]');
                const mid = Number(row?.getAttribute('data-mid'));
                closePinsModal();
                jumpToMessage(mid);
            });
        });

        body.querySelectorAll('[data-act="unpin"]').forEach((btn) => {
            btn.addEventListener('click', async (e) => {
                const row = e.target?.closest?.('[data-mid]');
                const mid = Number(row?.getAttribute('data-mid'));
                if (!Number.isFinite(mid) || mid <= 0) return;
                try {
                    await api(`/api/messages/${mid}/pin`, { method: 'DELETE' });
                } catch (err) {
                    console.warn('[PINS] unpin failed', err);
                }
                openPinsModal();
            });
        });
    } catch (e) {
        console.warn('[PINS] load failed', e);
        body.innerHTML = '<div class="muted">Не удалось загрузить</div>';
    }
}



// ===== DM CALLS (voice in DMs) =====
let dmCallIncoming = null; // {chat_id, from_user_id, from_username}
let dmCallOverlayMode = 'incoming';

function dmCallOverlayEls() {
    return {
        overlay: document.getElementById('dmCallOverlay'),
        card: document.querySelector('#dmCallOverlay .dm-call-card'),
        avatar: document.getElementById('dmCallAvatar'),
        name: document.getElementById('dmCallName'),
        title: document.getElementById('dmCallTitle'),
        sub: document.getElementById('dmCallSub'),
        accept: document.getElementById('dmCallAcceptBtn'),
        decline: document.getElementById('dmCallDeclineBtn'),
    };
}

function setDmCallOverlay(info, mode = 'incoming') {
    const { overlay, avatar, name, title, sub, accept, decline } = dmCallOverlayEls();
    if (!overlay) return;

    dmCallIncoming = info || null;
    dmCallOverlayMode = mode || 'incoming';

    const displayName = (info?.from_username || info?.target_username || chatNameById.get(Number(info?.chat_id)) || 'Пользователь').toString();
    const letter = (displayName.trim().charAt(0) || 'U').toUpperCase();

    if (avatar) avatar.textContent = letter;
    if (name) name.textContent = displayName;

    if (mode === 'outgoing') {
        if (title) title.textContent = 'Исходящий звонок';
        if (sub) sub.textContent = 'Ожидаем ответ…';
        if (accept) accept.hidden = true;
        if (decline) decline.textContent = 'Отменить';
    } else {
        if (title) title.textContent = 'Входящий звонок';
        if (sub) sub.textContent = 'Принять звонок?';
        if (accept) accept.hidden = false;
        if (decline) decline.textContent = 'Отклонить';
    }

    overlay.dataset.mode = mode;
    overlay.classList.remove('hidden');
    overlay.setAttribute('aria-hidden', 'false');
}

function showDmCallOverlay(info) {
    setDmCallOverlay(info, 'incoming');
}

function hideDmCallOverlay() {
    const { overlay, accept, decline } = dmCallOverlayEls();
    if (!overlay) return;
    overlay.classList.add('hidden');
    overlay.setAttribute('aria-hidden', 'true');
    overlay.dataset.mode = 'incoming';
    if (accept) accept.hidden = false;
    if (decline) decline.textContent = 'Отклонить';
    dmCallIncoming = null;
    dmCallOverlayMode = 'incoming';
}

async function startDmCall() {
    if (currentServerId) return; // only DMs
    const chatId = Number(currentChatId);
    if (!Number.isFinite(chatId) || chatId <= 0) return;

    const otherName = (chatNameById.get(chatId) || '').toString() || 'DM';

    try {
        wsManager.send({ type: 'dm_call_invite', data: { chat_id: chatId } });
    } catch (_) {}

    setDmCallOverlay({ chat_id: chatId, target_username: otherName }, 'outgoing');
    showToast('Вызов отправлен');
}

function wireDmCallOverlayButtonsOnce() {
    const { accept, decline, overlay } = dmCallOverlayEls();
    if (overlay && overlay.dataset.wired === '1') return;
    if (overlay) overlay.dataset.wired = '1';

    overlay?.addEventListener('click', (e) => {
        if (e.target !== overlay) return;
        if (dmCallOverlayMode === 'outgoing' && dmCallIncoming?.chat_id) {
            try { wsManager.send({ type: 'dm_call_cancel', data: { chat_id: Number(dmCallIncoming.chat_id), reason: 'dismissed' } }); } catch (_) {}
        }
        hideDmCallOverlay();
    });

    accept?.addEventListener('click', async (e) => {
        e.preventDefault();
        e.stopPropagation();
        const info = dmCallIncoming;
        if (!info) return;
        const chatId = Number(info.chat_id);
        const fromName = (info.from_username || 'Unknown').toString();
        hideDmCallOverlay();

        try { wsManager.send({ type: 'dm_call_accept', data: { chat_id: chatId } }); } catch (_) {}

        try {
            await openDmChat(chatId, fromName);
        } catch (_) {}

        try {
            await window.lbVoice?.join?.(chatId, fromName);
        } catch (_) {}
    });

    decline?.addEventListener('click', (e) => {
        e.preventDefault();
        e.stopPropagation();
        const info = dmCallIncoming;
        if (!info) {
            hideDmCallOverlay();
            return;
        }
        const chatId = Number(info.chat_id);
        const evt = dmCallOverlayMode === 'outgoing' ? 'dm_call_cancel' : 'dm_call_decline';
        const reason = dmCallOverlayMode === 'outgoing' ? 'canceled' : 'declined';
        hideDmCallOverlay();
        try { wsManager.send({ type: evt, data: { chat_id: chatId, reason } }); } catch (_) {}
    });
}

function onDmCallEvent(ev) {
    const t = (ev?.type || '').toString();
    if (!t.startsWith('dm_call_')) return;

    wireDmCallOverlayButtonsOnce();

    if (t === 'dm_call_invite') {
        showDmCallOverlay({
            chat_id: ev.chat_id,
            from_user_id: ev.from_user_id,
            from_username: ev.from_username
        });
        return;
    }

    if (t === 'dm_call_cancel') {
        hideDmCallOverlay();
        showToast('Вызов отменён');
        return;
    }

    if (t === 'dm_call_decline') {
        hideDmCallOverlay();
        showToast('Вызов отклонён');
        return;
    }

    if (t === 'dm_call_accept') {
        // remote accepted: auto-join voice if we are still in that DM
        const chatId = Number(ev.chat_id);
        const fromName = (ev.from_username || chatNameById.get(chatId) || 'DM').toString();
        hideDmCallOverlay();
        showToast('Вызов принят');
        try {
            openDmChat(chatId, fromName).catch(() => {});
            window.lbVoice?.join?.(chatId, fromName);
        } catch (_) {}
        return;
    }

    if (t === 'dm_call_end') {
        const chatId = Number(ev.chat_id);
        hideDmCallOverlay();
        showToast('Звонок завершён');
        try {
            const st = window.lbVoice?.getState?.();
            const inCh = Number(st?.channel_id || 0);
            if (inCh && inCh === chatId) {
                window.lbVoice?.leave?.();
            }
        } catch (_) {}
        return;
    }

    // *_sent
    if (t.endsWith('_sent')) {
        return;
    }
}

window.onDmCallEvent = onDmCallEvent;

async function refreshFriendsStatus() {
    const el = document.getElementById('friendsStatus');
    if (!el) return;
    try {
        const friends = await api('/api/friends');
        const total = Array.isArray(friends) ? friends.length : 0;
        const online = Array.isArray(friends)
            ? friends.filter(f => (f.status && f.status !== 'offline') || f.is_online).length
            : 0;
        el.textContent = `${online}/${total} в сети`;
    } catch (_) {
        el.textContent = '—';
    }
}

function askConfirmModal(opts = {}) {
    const title = (opts.title || 'Подтверждение').toString();
    const text = (opts.text || '').toString();
    const okText = (opts.okText || 'Подтвердить').toString();
    const cancelText = (opts.cancelText || 'Отмена').toString();
    const danger = Boolean(opts.danger);

    return new Promise((resolve) => {
        const overlay = document.createElement('div');
        overlay.className = 'modal-overlay';
        overlay.id = 'confirmOverlay';

        overlay.innerHTML = `
          <div class="modal" role="dialog" aria-modal="true">
            <div class="modal-header">
              <h2>${escapeHtml(title)}</h2>
              <button class="icon-btn" type="button" id="confirmCloseBtn">✕</button>
            </div>
            <div class="modal-body">
              <div>${escapeHtml(text)}</div>
            </div>
            <div class="modal-actions">
              <button class="btn btn-ghost" type="button" id="confirmCancelBtn">${escapeHtml(cancelText)}</button>
              <button class="btn ${danger ? 'btn-danger' : 'btn-primary'}" type="button" id="confirmOkBtn">${escapeHtml(okText)}</button>
            </div>
          </div>
        `;

        const cleanup = (val) => {
            try { overlay.remove(); } catch (_) {}
            resolve(Boolean(val));
        };

        overlay.addEventListener('click', (e) => {
            if (e.target === overlay) cleanup(false);
        });

        document.body.appendChild(overlay);

        const btnOk = overlay.querySelector('#confirmOkBtn');
        const btnCancel = overlay.querySelector('#confirmCancelBtn');
        const btnClose = overlay.querySelector('#confirmCloseBtn');

        btnOk?.addEventListener('click', () => cleanup(true));
        btnCancel?.addEventListener('click', () => cleanup(false));
        btnClose?.addEventListener('click', () => cleanup(false));

        overlay.addEventListener('keydown', (e) => {
            if (e.key === 'Escape') cleanup(false);
            if (e.key === 'Enter') cleanup(true);
        });

        setTimeout(() => {
            try { btnOk?.focus(); } catch (_) {}
        }, 0);
    });
}

function formatPinTimestamp(value) {
    const raw = (value ?? '').toString().trim();
    if (!raw) return 'Дата неизвестна';

    const asNumber = Number(raw);
    if (Number.isFinite(asNumber) && asNumber > 0) {
        const ms = raw.length >= 13 ? asNumber : asNumber * 1000;
        const dt = new Date(ms);
        if (!Number.isNaN(dt.getTime())) {
            return dt.toLocaleString('ru-RU', {
                day: '2-digit',
                month: '2-digit',
                year: 'numeric',
                hour: '2-digit',
                minute: '2-digit',
            });
        }
    }

    const iso = new Date(raw);
    if (!Number.isNaN(iso.getTime())) {
        return iso.toLocaleString('ru-RU', {
            day: '2-digit',
            month: '2-digit',
            year: 'numeric',
            hour: '2-digit',
            minute: '2-digit',
        });
    }

    return raw;
}

function askTextModal(opts = {}) {
    const title = (opts.title || 'Введите значение').toString();
    const label = (opts.label || '').toString();
    const placeholder = (opts.placeholder || '').toString();
    const okText = (opts.okText || 'OK').toString();
    const cancelText = (opts.cancelText || 'Отмена').toString();
    const initial = (opts.value || '').toString();

    return new Promise((resolve) => {
        const overlay = document.createElement('div');
        overlay.className = 'modal-overlay';
        overlay.id = 'promptOverlay';

        overlay.innerHTML = `
          <div class="modal" role="dialog" aria-modal="true">
            <div class="modal-header">
              <h2>${escapeHtml(title)}</h2>
              <button class="icon-btn" type="button" id="promptCloseBtn">✕</button>
            </div>
            <div class="modal-body">
              ${label ? `<div class="muted" style="margin-bottom:8px;">${escapeHtml(label)}</div>` : ''}
              <input class="inp" id="promptInput" placeholder="${escapeHtml(placeholder)}" autocomplete="off">
            </div>
            <div class="modal-actions">
              <button class="btn btn-ghost" type="button" id="promptCancelBtn">${escapeHtml(cancelText)}</button>
              <button class="btn btn-primary" type="button" id="promptOkBtn">${escapeHtml(okText)}</button>
            </div>
          </div>
        `;

        const cleanup = (val) => {
            try { overlay.remove(); } catch (_) {}
            resolve(val);
        };

        overlay.addEventListener('click', (e) => {
            if (e.target === overlay) cleanup(null);
        });

        document.body.appendChild(overlay);

        const input = overlay.querySelector('#promptInput');
        const btnOk = overlay.querySelector('#promptOkBtn');
        const btnCancel = overlay.querySelector('#promptCancelBtn');
        const btnClose = overlay.querySelector('#promptCloseBtn');

        if (input) {
            input.value = initial;
            setTimeout(() => { try { input.focus(); input.select?.(); } catch (_) {} }, 0);
            input.addEventListener('keydown', (e) => {
                if (e.key === 'Enter') {
                    e.preventDefault();
                    btnOk?.click();
                }
                if (e.key === 'Escape') {
                    e.preventDefault();
                    btnCancel?.click();
                }
            });
        }

        btnOk?.addEventListener('click', () => {
            const v = (input?.value || '').trim();
            cleanup(v || null);
        });

        const onCancel = () => cleanup(null);
        btnCancel?.addEventListener('click', onCancel);
        btnClose?.addEventListener('click', onCancel);
    });
}

async function createServerFlow() {
    const name = await askTextModal({
        title: 'Создать сервер',
        label: 'Название сервера',
        placeholder: 'Например: Global',
        okText: 'Создать',
        cancelText: 'Отмена',
    });

    if (!name) return;

    try {
        const res = await api('/api/servers', {
            method: 'POST',
            body: JSON.stringify({ name })
        });
        const servers = await api('/api/servers');
        renderServers(servers);
        const newId = Number(res?.id);
        const idToOpen = Number.isFinite(newId) && newId > 0 ? newId : (servers[0]?.id || null);
        const srv = servers.find(x => x.id === idToOpen);
        if (idToOpen) await openServer(idToOpen, srv?.name || name);
    } catch (e) {
        console.error('[UI] Failed to create server', e);
        alert('Не удалось создать сервер');
    }
}

async function loadMe() {
    try {
        console.log("[UI] Loading current user...");
        const me = await api("/api/users/me");
        if (!me || typeof me !== 'object') {
            throw new Error('Invalid /api/users/me response');
        }
        currentUser = me;
        const displayName = (me.nickname || me.username || '').toString();
        $("userName").textContent = displayName || 'Unknown';
        const avatarTextEl = $("avatarText");
        if (avatarTextEl) avatarTextEl.textContent = (displayName.charAt(0) || '?').toUpperCase();
        try {
            currentUserProfile = await api("/api/users/me/profile");
        } catch (_) {
            currentUserProfile = null;
        }
        applyUserbarAvatar();
        console.log(`[ME] Loaded as ${currentUser.username}`);
    } catch (e) {
        console.error("[ME] Failed to load current user", e);
        
        if (e.status === 401 || e.message.includes('401')) {
            console.error('[ME] Token invalid or expired, redirecting to login');
            localStorage.removeItem('auth_token');
            localStorage.removeItem('refresh_token');
            sessionStorage.clear();
            window.location.href = "/";
            return;
        }
        
        throw e;
    }
}

function showChannelsMenu() {
    const channelsPanel = document.querySelector('.panel.channels');
    if (channelsPanel) {
        channelsPanel.classList.add('show-channels');
        document.body.classList.add('channels-open');
        hideServersMenu();
        hideMembersMenu();
        console.log('[UI] Channels menu shown');
    }
}

function hideChannelsMenu() {
    const channelsPanel = document.querySelector('.panel.channels');
    if (channelsPanel) {
        channelsPanel.classList.remove('show-channels');
        document.body.classList.remove('channels-open');
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

function isTouchUi() {
    try {
        return window.matchMedia('(pointer: coarse)').matches || window.innerWidth <= 600;
    } catch (_) {
        return window.innerWidth <= 600;
    }
}

function showServersMenu() {
    const serversPanel = document.querySelector('.panel.servers');
    if (serversPanel) {
        serversPanel.classList.add('show-servers');
        document.body.classList.add('servers-open');
        // mutually exclusive
        hideChannelsMenu();
        hideMembersMenu();
    }
}

function hideServersMenu() {
    const serversPanel = document.querySelector('.panel.servers');
    if (serversPanel) {
        serversPanel.classList.remove('show-servers');
        document.body.classList.remove('servers-open');
    }
}

function toggleServersMenu() {
    const serversPanel = document.querySelector('.panel.servers');
    if (!serversPanel) return;
    const isVisible = serversPanel.classList.contains('show-servers');
    if (isVisible) hideServersMenu();
    else showServersMenu();
}

function showMembersMenu() {
    const membersPanel = document.querySelector('.panel.members');
    if (membersPanel) {
        membersPanel.classList.add('show-members');
        document.body.classList.add('members-open');
        hideChannelsMenu();
        hideServersMenu();
    }
}

function hideMembersMenu() {
    const membersPanel = document.querySelector('.panel.members');
    if (membersPanel) {
        membersPanel.classList.remove('show-members');
        document.body.classList.remove('members-open');
    }
}

function toggleMembersMenu() {
    const membersPanel = document.querySelector('.panel.members');
    if (!membersPanel) return;
    const isVisible = membersPanel.classList.contains('show-members');
    if (isVisible) hideMembersMenu();
    else showMembersMenu();
}

function closeAllDrawers() {
    hideChannelsMenu();
    hideServersMenu();
    hideMembersMenu();
}

function ensureReplyBar() {
    if (replyBarEl) return replyBarEl;
    const composer = document.getElementById('composer');
    if (!composer) return null;

    const bar = document.createElement('div');
    bar.className = 'replybar';
    bar.hidden = true;
    bar.innerHTML = `
      <div class="rb-left">
        <div class="rb-title">Ответ</div>
        <div class="rb-preview" id="rbPreview"></div>
      </div>
      <button type="button" class="icon-btn" id="rbCancel" title="Отменить">✕</button>
    `;
    composer.insertBefore(bar, composer.firstChild);

    bar.querySelector('#rbCancel')?.addEventListener('click', () => {
        clearReplyTo();
    });

    replyBarEl = bar;
    return bar;
}

function clearReplyTo() {
    replyToMessageId = null;
    replyToPreview = null;
    if (replyBarEl) replyBarEl.hidden = true;
}

function setReplyTo(messageId, sender, text) {
    const id = Number(messageId);
    if (!Number.isFinite(id) || id <= 0) return;
    replyToMessageId = id;
    replyToPreview = { sender: (sender || '').toString(), text: (text || '').toString() };

    const bar = ensureReplyBar();
    if (!bar) return;
    const prev = bar.querySelector('#rbPreview');
    if (prev) {
        const s = replyToPreview.sender ? `${replyToPreview.sender}: ` : '';
        prev.textContent = (s + replyToPreview.text).slice(0, 200);
    }
    bar.hidden = false;
}

function ensureEmojiPicker() {
    if (emojiPickerEl && emojiPickerBackdrop) return;

    emojiPickerBackdrop = document.createElement('div');
    emojiPickerBackdrop.className = 'emoji-backdrop';
    emojiPickerBackdrop.hidden = true;

    emojiPickerEl = document.createElement('div');
    emojiPickerEl.className = 'emoji-picker';
    emojiPickerEl.hidden = true;

    const emojis = [
        '😀','😁','😂','🤣','😊','😍','😘','😎','🤔','😴','😭','😡','👍','👎','🙏','👏','🔥','💯','🎉','❤️','💔','✅','❌','⭐','⚡','🍀','🎧','🎮','📌','📎'
    ];

    emojiPickerEl.innerHTML = `
      <div class="emoji-grid">
        ${emojis.map(e => `<button type="button" class="emoji-btn" data-emoji="${escapeHtml(e)}">${escapeHtml(e)}</button>`).join('')}
      </div>
    `;

    emojiPickerEl.addEventListener('click', async (e) => {
        const btn = e.target?.closest?.('.emoji-btn');
        if (!btn) return;
        const emoji = btn.getAttribute('data-emoji') || '';
        const mid = Number(emojiPickerEl.dataset.forMsgId);
        if (!emoji || !Number.isFinite(mid) || mid <= 0) return;
        try {
            await api(`/api/messages/${mid}/reactions/${encodeURIComponent(emoji)}`, { method: 'PUT' });
            refreshMessageReactions(mid, { force: true });
        } catch (err) {
            console.warn('[UI] add reaction failed', err);
        } finally {
            hideEmojiPicker();
        }
    });

    emojiPickerBackdrop.addEventListener('click', () => hideEmojiPicker());
    document.addEventListener('keydown', (e) => {
        if (e.key === 'Escape') hideEmojiPicker();
    });

    document.body.appendChild(emojiPickerBackdrop);
    document.body.appendChild(emojiPickerEl);
}

function hideEmojiPicker() {
    if (emojiPickerBackdrop) emojiPickerBackdrop.hidden = true;
    if (emojiPickerEl) emojiPickerEl.hidden = true;
    try { emojiPickerEl.dataset.forMsgId = ''; } catch (_) {}
}

function showEmojiPicker({ anchorEl, messageId } = {}) {
    ensureEmojiPicker();
    const mid = Number(messageId);
    if (!Number.isFinite(mid) || mid <= 0) return;
    if (!emojiPickerEl || !emojiPickerBackdrop) return;

    emojiPickerEl.dataset.forMsgId = String(mid);

    // place
    let x = window.innerWidth / 2;
    let y = window.innerHeight / 2;
    if (anchorEl && anchorEl.getBoundingClientRect) {
        const r = anchorEl.getBoundingClientRect();
        x = r.left;
        y = r.top - 8;
    }

    emojiPickerEl.style.left = '0px';
    emojiPickerEl.style.top = '0px';
    emojiPickerEl.hidden = false;

    const pad = 10;
    const w = emojiPickerEl.offsetWidth || 260;
    const h = emojiPickerEl.offsetHeight || 220;
    const px = Math.max(pad, Math.min(x, window.innerWidth - w - pad));
    const py = Math.max(pad, Math.min(y - h, window.innerHeight - h - pad));

    emojiPickerEl.style.left = px + 'px';
    emojiPickerEl.style.top = py + 'px';

    emojiPickerBackdrop.hidden = false;
}

// reactions (message-level)
const _lbReactionsFetched = new Set();
const _lbReactionsInFlight = new Set();

function getMessageElementById(messageId) {
    const mid = Number(messageId);
    if (!Number.isFinite(mid) || mid <= 0) return null;
    return document.querySelector(`.message[data-msg-id="${mid}"]`);
}

function renderReactionsHtml(items) {
    const arr = Array.isArray(items) ? items : [];
    if (!arr.length) return '';
    const pills = arr
        .filter((it) => it && it.emoji)
        .map((it) => {
            const emoji = (it.emoji || '').toString();
            const cnt = Number(it.count) || 0;
            const me = !!it.me;
            return `<button type="button" class="react-pill ${me ? 'me' : ''}" data-emoji="${escapeHtml(emoji)}" data-me="${me ? '1' : '0'}" title="Реакция">${escapeHtml(emoji)}<span class="cnt">${escapeHtml(String(cnt))}</span></button>`;
        })
        .join('');

    // add reaction button appears after first reaction
    const addBtn = `<button type="button" class="react-add" data-act="emoji" title="Добавить реакцию">+</button>`;
    return pills + addBtn;
}

function applyReactionsToMessageEl(msgEl, items) {
    if (!msgEl) return;
    const box = msgEl.querySelector('.msg-reactions');
    if (!box) return;
    const html = renderReactionsHtml(items);
    if (!html) {
        box.innerHTML = '';
        box.hidden = true;
        return;
    }
    box.innerHTML = html;
    box.hidden = false;
}

async function fetchMessageReactions(messageId) {
    const mid = Number(messageId);
    if (!Number.isFinite(mid) || mid <= 0) return [];
    const resp = await api(`/api/messages/${mid}/reactions`, { method: 'GET' });
    const items = resp?.items;
    return Array.isArray(items) ? items : [];
}

function refreshMessageReactions(messageId, { force = false } = {}) {
    const mid = Number(messageId);
    if (!Number.isFinite(mid) || mid <= 0) return;
    if (!force && _lbReactionsFetched.has(mid)) return;
    if (_lbReactionsInFlight.has(mid)) return;

    const el = getMessageElementById(mid);
    if (!el) return;

    _lbReactionsInFlight.add(mid);
    fetchMessageReactions(mid)
        .then((items) => {
            _lbReactionsFetched.add(mid);
            applyReactionsToMessageEl(el, items);
        })
        .catch((err) => console.warn('[UI] reactions fetch failed', err))
        .finally(() => _lbReactionsInFlight.delete(mid));
}


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

    try { serverOwnerById.clear(); } catch (_) {}
    
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
        try { serverOwnerById.set(Number(server.id), Number(server.owner_id)); } catch (_) {}
        const serverItem = document.createElement('div');
        const isActive = server.id === currentServerId;
        serverItem.className = `item server ${isActive ? 'active' : ''}`;
        serverItem.dataset.serverId = server.id;
        serverItem.dataset.testId = `server-${server.id}`;
        
        const serverDesc = (server.description || '').toString().trim();
        const isOwner = Number(server.owner_id) === Number(currentUser?.id);
        serverItem.innerHTML = `
            <div class="avatar">${(server.name || 'S')[0]?.toUpperCase() || 'S'}</div>
            <div class="text">
                <div class="title">${escapeHtml((server.name || '').toString())}</div>
                ${serverDesc ? `<div class="sub">${escapeHtml(serverDesc)}</div>` : ``}
            </div>
            ${isOwner ? `<button class="server-del" type="button" title="Удалить сервер">🗑</button>` : ``}
        `;

        const delBtn = serverItem.querySelector('.server-del');
        delBtn?.addEventListener('click', async (e) => {
            e.preventDefault();
            e.stopPropagation();
            if (!confirm('Удалить сервер?')) return;
            try {
                await api(`/api/servers/${server.id}`, { method: 'DELETE' });
                window.location.reload();
            } catch (err) {
                console.warn('[UI] delete server failed', err);
                showToast('Не удалось удалить сервер');
            }
        });

        serverItem.addEventListener('click', (e) => {
            e.stopPropagation();
            e.preventDefault();
            
            console.log('[CLICK] Server clicked:', {
                id: server.id,
                name: server.name,
                currentServerId,
                isActive
            });
            
            if (currentServerId === server.id) {
                console.log(`[UI] Server ${server.id} already active`);

                // if we are in Friends view, close it and ensure server UI is visible
                if (location.hash === '#/friends' && window.closeFriends) {
                    window.closeFriends();
                    try { history.replaceState(null, '', location.pathname + location.search); } catch (_) { location.hash = ''; }
                    // force refresh of channels/chat
                    openServer(server.id, server.name);
                    if (isTouchUi()) {
                        hideServersMenu();
                        hideChannelsMenu();
                        hideMembersMenu();
                    }
                    return;
                }

                serverItem.classList.add('refreshing');
                setTimeout(() => serverItem.classList.remove('refreshing'), 300);

                if (isTouchUi()) {
                    hideServersMenu();
                    hideChannelsMenu();
                    hideMembersMenu();
                }

                return;
            }
            
            console.log(`[UI] Opening server ${server.id} (${server.name})`);
            openServer(server.id, server.name);
            if (isTouchUi()) {
                hideServersMenu();
                hideChannelsMenu();
                hideMembersMenu();
            }
        });
        
        serversList.appendChild(serverItem);
    });
    
    
console.log('[DEBUG] Servers rendered:', serversList.children.length);
}


function canManageChannels(serverId) {
    const sid = Number(serverId);
    if (!Number.isFinite(sid) || sid <= 0) return false;
    const ownerId = serverOwnerById.get(sid);
    return Number(ownerId) === Number(currentUser?.id);
}

function updateChannelAdminUi() {
    const btn = document.getElementById('addChannelBtn');
    if (!btn) return;
    btn.style.display = canManageChannels(currentServerId) ? '' : 'none';
}

async function createChannelFlow() {
    if (!currentServerId) return;
    if (!canManageChannels(currentServerId)) return;

    const name = await askTextModal({
        title: 'Создать канал',
        label: 'Название канала',
        placeholder: 'Например: general',
        okText: 'Дальше',
        cancelText: 'Отмена',
    });
    if (!name) return;

    const isVoice = confirm('Создать голосовой канал?\nOK — голосовой\nОтмена — текстовый');
    const kind = isVoice ? 'voice' : 'text';

    try {
        const res = await api(`/api/servers/${currentServerId}/chats`, {
            method: 'POST',
            body: JSON.stringify({ name, kind })
        });

        const chats = await api(`/api/servers/${currentServerId}/chats`);
        renderChannels(chats);
        updateChannelAdminUi();

        const newId = Number(res?.id);
        const chatId = (Number.isFinite(newId) && newId > 0)
            ? newId
            : (chats.find(c => (c?.name || '') === name && (c?.kind || 'text') === kind)?.id);

        if (chatId) {
            const chat = chats.find(c => c.id === chatId);
            await openChat(chatId, chat?.name || name);
            if (kind === 'voice') {
                try { await window.lbVoice?.join?.(chatId, chat?.name || name); } catch (_) {}
            }
        }
    } catch (e) {
        console.warn('[UI] create channel failed', e);
        showToast('Не удалось создать канал');
    }
}


async function loadMembers(serverId) {
    const membersList = $('membersList');
    const membersPanel = $('membersPanel');
    const countEl = membersPanel?.querySelector('.count');
    if (!membersList) return;

    try {
        const members = await api(`/api/servers/${serverId}/members`);
        renderMembers(members);
        if (countEl) countEl.textContent = `(${members?.length || 0})`;
    } catch (err) {
        console.warn('[UI] Failed to load members', err);
        membersList.innerHTML = `<div class="muted" style="padding:12px;">Не удалось загрузить участников</div>`;
        if (countEl) countEl.textContent = `(0)`;
    }
}

function renderMembers(members) {
    const membersList = $('membersList');
    if (!membersList) return;

    membersList.innerHTML = '';
    if (!members || members.length === 0) {
        membersList.innerHTML = `<div class="muted" style="padding:12px;">Нет участников</div>`;
        return;
    }

    const onlineMembers = [];
    const offlineMembers = [];

    for (const m of members) {
        const rawStatus = m.status || (m.is_online ? 'online' : 'offline');
        let st = statusToClass(rawStatus);
        if (st === 'invisible') st = 'offline';
        if (st === 'offline') offlineMembers.push(m);
        else onlineMembers.push(m);
    }

    const createMemberEl = (m) => {
        const el = document.createElement('div');
        const rawStatus = m.status || (m.is_online ? 'online' : 'offline');
        let st = statusToClass(rawStatus);
        if (st === 'invisible') st = 'offline';
        const online = st !== 'offline';
        const badgeHtml = m.role === 'admin' ? '<span class="member-badge">Админ</span>' : '';

        el.className = `member status-${st} ${online ? 'online' : 'offline'}`;
        el.innerHTML = `
          <div class="avatar small">${avatarInnerHtml(m.avatar_file_id, m.username)}</div>
          <div class="text">
            <div class="name">${escapeHtml(m.username || 'Unknown')}</div>
            <div class="role">${badgeHtml}</div>
          </div>
        `;

        el.dataset.userId = String(m.id);
        el.dataset.username = (m.username || '').toString();

        el.addEventListener('click', (e) => {
            const uid = Number(m?.id);
            if (!Number.isFinite(uid) || uid <= 0) return;
            if (uid === (currentUser?.id || -1)) return;

            const anchor = e?.target?.closest?.('.avatar') || el;

            e.stopPropagation();
            showUserMenu({
                userId: uid,
                username: (m.username || 'Unknown').toString(),
                anchorEl: anchor,
                allowDm: true,
                allowAddFriend: true,
                allowRemoveFriend: false,
            });
        });

        return el;
    };

    const renderGroup = (title, arr) => {
        if (!arr.length) return;

        const h = document.createElement('div');
        h.className = 'members-group-title';
        h.innerHTML = `<span class="t">${escapeHtml(title)} — ${arr.length}</span>`;
        membersList.appendChild(h);

        const box = document.createElement('div');
        box.className = 'members-group-box';

        for (const m of arr) {
            box.appendChild(createMemberEl(m));
        }
        membersList.appendChild(box);
    };

    renderGroup('В сети', onlineMembers);
    renderGroup('Не в сети', offlineMembers);
}

async function openServer(serverId, serverName) {
    if (isOpeningServer) {
        console.log('[UI] Server opening in progress, skipping');
        return;
    }
    
    isOpeningServer = true;
    
    try {
        console.log(`[UI] Opening server ${serverId} (${serverName})`);
        if ((location.hash === '#/friends' || location.hash === '#friends') && window.closeFriends) {
            window.closeFriends();
            // clear hash so friends doesn't reopen on refresh
            try { history.replaceState(null, '', location.pathname + location.search); } catch (_) { location.hash = ''; }
        }
        
        setUiModeServer();
        currentServerId = serverId;
        sessionStorage.setItem("lastServerId", serverId.toString());
        
        console.log('[DEBUG] State updated:', {
            currentServerId,
            sessionStorage: sessionStorage.getItem("lastServerId")
        });
        
        document.querySelectorAll('.item.server').forEach(item => {
            const itemId = parseInt(item.dataset.serverId);
            item.classList.toggle('active', itemId === serverId);
        });
        
        const chats = await api(`/api/servers/${serverId}/chats`);
        console.log(`[UI] Loaded ${chats.length} chats for server ${serverId}`);
        
        renderChannels(chats);
        // update members list
        await loadMembers(serverId);
        
        if (window.innerWidth <= 900 || isTouchUi()) {
            hideServersMenu();
            hideMembersMenu();
        }
        
        const lastChatId = Number(sessionStorage.getItem("lastChatId"));
        const restored = chats.find(c => c.id === lastChatId && c.kind !== 'voice')?.id;
        const firstText = chats.find(c => c.kind !== 'voice')?.id;
        const chatId = restored ?? firstText ?? chats[0]?.id;
        
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
        updateChannelAdminUi();
        return;
    }

    const canManage = canManageChannels(currentServerId);

    chats.forEach(chat => {
        try { chatNameById.set(chat.id, chat.name || `#${chat.id}`); } catch (_) {}
        try { chatKindById.set(chat.id, (chat && chat.kind) ? chat.kind : 'text'); } catch (_) {}

        const channelItem = document.createElement('div');
        const isActive = chat.id === currentChatId;
        channelItem.dataset.channelId = chat.id;

        const isVoice = (chat && chat.kind === 'voice');
        const icon = isVoice ? '🔊' : '#';
        channelItem.className = `item channel ${isVoice ? 'voice' : ''} ${isActive ? 'active' : ''}`;

        const unread = Number(chat?.unread_count ?? 0);
        // unread badges only for TEXT channels
        const hasUnread = !isVoice && Number.isFinite(unread) && unread > 0;

        // IMPORTANT: last message preview should NOT be shown in public server channels.
        const subText = isVoice
            ? 'Голосовой канал'
            : (chat.description || 'Канал');

        const delBtn = canManage ? `<button class="channel-del" type="button" title="Удалить канал">🗑</button>` : '';

        channelItem.innerHTML = `
            <span class="hash">${icon}</span>
            <div class="text">
                <div class="title">
                  <span class="title-text">${escapeHtml(chat.name || '')}</span>
                  ${hasUnread ? `<span class="badge-unread" title="Непрочитано">${unread > 99 ? '99+' : unread}</span>` : ''}
                </div>
                <div class="sub">${escapeHtml(subText)}</div>
                ${isVoice ? `<div class="voice-users" hidden></div>` : ''}
            </div>
            ${delBtn}
        `;

        // delete channel (owner/admin)
        channelItem.querySelector('.channel-del')?.addEventListener('click', async (e) => {
            e.preventDefault();
            e.stopPropagation();
            if (!currentServerId) return;
            if (!confirm('Удалить канал?')) return;
            try {
                await api(`/api/servers/${currentServerId}/chats/${chat.id}`, { method: 'DELETE' });
            } catch (err) {
                const d = err?.detail || err?.message || '';
                if (String(d).includes('cannot_delete_last_of_kind')) {
                    showToast('Нельзя удалить последний канал этого типа');
                } else {
                    showToast('Не удалось удалить канал');
                }
                return;
            }
            try {
                const chats = await api(`/api/servers/${currentServerId}/chats`);
                renderChannels(chats);
                updateChannelAdminUi();
                if (Number(currentChatId) === Number(chat.id)) {
                    const firstText = chats.find(c => c.kind !== 'voice')?.id;
                    const nextId = firstText ?? chats[0]?.id;
                    if (nextId) {
                        const c = chats.find(x => x.id === nextId);
                        await openChat(nextId, c?.name || 'Unknown');
                    }
                }
            } catch (_) {}
        });

        channelItem.addEventListener('click', async () => {
            const isVoice = (chat && chat.kind === 'voice');
            if (isVoice) {
                const targetId = Number(chat.id);
                const targetName = (chat.name || 'Voice').toString();

                const st = window.lbVoice?.getState?.();
                const inCh = Number(st?.channel_id || 0);

                // Not in voice yet: open voice view and connect
                if (!inCh) {
                    try { await openVoiceView(targetId, targetName); } catch (_) {}
                    try { await window.lbVoice?.join?.(targetId, targetName); } catch (_) {}
                    markVoiceSelectedInList(targetId);
                    return;
                }

                // Already in this voice: just open the voice view
                if (inCh === targetId) {
                    try { await openVoiceView(targetId, targetName); } catch (_) {}
                    markVoiceSelectedInList(targetId);
                    return;
                }

                // Switching voice: require double click to avoid misclick disconnect
                const now = Date.now();
                if (lastVoiceSwitchClick?.id === targetId && (now - Number(lastVoiceSwitchClick.at || 0)) < 1500) {
                    lastVoiceSwitchClick = { id: null, at: 0 };
                    try { await openVoiceView(targetId, targetName); } catch (_) {}
                    try { await window.lbVoice?.join?.(targetId, targetName); } catch (_) {}
                    markVoiceSelectedInList(targetId);
                } else {
                    lastVoiceSwitchClick = { id: targetId, at: now };
                    markVoiceSelectedInList(targetId);
                    showToast('Нажмите ещё раз, чтобы перейти в другой голосовой');
                }

                return;
            }
            openChat(chat.id, chat.name);
        });

        channelsList.appendChild(channelItem);
    });

    updateChannelAdminUi();
}

let currentVoiceViewChannelId = null;

// chatView is moved between chatPanel and membersPanel in voice split mode
let chatViewHomeParent = null;
let chatViewHomeNext = null;

function setVoiceDiscordLayout(enabled) {
  const chatView = document.getElementById('chatView');
  const voiceChatSide = document.getElementById('voiceChatSide');
  const membersPanelMembers = document.getElementById('membersPanelMembers');
  const membersPanel = document.getElementById('membersPanel');

  if (!chatView || !voiceChatSide || !membersPanel) return;

  if (enabled) {
    if (!chatViewHomeParent) {
      chatViewHomeParent = chatView.parentElement;
      chatViewHomeNext = chatView.nextElementSibling;
    }

    // move chat to the right panel
    try {
      voiceChatSide.hidden = false;
      if (membersPanelMembers) membersPanelMembers.hidden = true;
      membersPanel.hidden = false;

      if (chatView.parentElement !== voiceChatSide) {
        voiceChatSide.appendChild(chatView);
      }
    } catch (_) {}
  } else {
    // restore default layout
    try {
      if (membersPanelMembers) membersPanelMembers.hidden = false;
      voiceChatSide.hidden = true;

      if (chatViewHomeParent && chatView.parentElement !== chatViewHomeParent) {
        if (chatViewHomeNext && chatViewHomeNext.parentElement === chatViewHomeParent) {
          chatViewHomeParent.insertBefore(chatView, chatViewHomeNext);
        } else {
          chatViewHomeParent.appendChild(chatView);
        }
      }
    } catch (_) {}
  }
}

function markVoiceSelectedInList(channelId) {
  const list = document.getElementById('channels-list');
  if (!list) return;
  const cid = Number(channelId);
  list.querySelectorAll('.item.channel.voice').forEach((it) => {
    const id = Number(it.dataset.channelId);
    it.classList.toggle('voice-selected', Number.isFinite(cid) && cid > 0 && id === cid);
  });
}

let voiceTextLockedChatId = null;

function showVoiceTextLocked(chatId, channelName) {
  const cid = Number(chatId);
  if (!Number.isFinite(cid) || cid <= 0) return;

  voiceTextLockedChatId = cid;
  currentChatId = cid;
  clearReplyTo();

  // header
  const chatTitleElement = document.getElementById('chat-title');
  if (chatTitleElement) {
    chatTitleElement.textContent = `# ${channelName || 'Voice'}`;
  }

  // active item highlight
  document.querySelectorAll('.item.channel').forEach(item => {
    const itemId = parseInt(item.dataset.channelId);
    item.classList.toggle('active', itemId === cid);
  });

  // lock screen
  const messagesContainer = document.getElementById('messages');
  if (messagesContainer) {
    messagesContainer.innerHTML = `
      <div class="voice-text-lock">
        <div class="ttl">Текстовый чат голосового канала</div>
        <div class="sub">Подключитесь к голосовому каналу, чтобы видеть сообщения и уведомления.</div>
        <div class="hint">Нажмите на голосовой канал и подключитесь.</div>
      </div>
    `;
  }

  // hide composer
  const composer = document.getElementById('composer');
  if (composer) composer.hidden = true;
}

function hideVoiceTextLocked() {
  voiceTextLockedChatId = null;
  const composer = document.getElementById('composer');
  if (composer) composer.hidden = false;
}

async function openVoiceView(channelId, channelName) {
  const voiceView = document.getElementById('voiceView');
  if (!voiceView) return;

  const chatView = document.getElementById('chatView');
  const friendsView = document.getElementById('friendsView');
  const membersPanel = document.getElementById('membersPanel');
  const voiceChatSide = document.getElementById('voiceChatSide');

  currentVoiceViewChannelId = Number(channelId);
  if (!Number.isFinite(currentVoiceViewChannelId) || currentVoiceViewChannelId <= 0) currentVoiceViewChannelId = null;
  if (!currentVoiceViewChannelId) return;

  // Discord-like: stage in center, text chat on the right.
  if (friendsView) friendsView.hidden = true;
  if (membersPanel) membersPanel.hidden = false;
  if (voiceChatSide) voiceChatSide.hidden = false;
  setVoiceDiscordLayout(true);

  voiceView.hidden = false;
  document.body.classList.add('voice-view-open');
  document.body.classList.add('voice-split-open');  // Open voice text only if we are actually connected to this voice channel.
  try {
    const stv = window.lbVoice?.getState?.();
    const inCh = Number(stv?.channel_id || 0);
    if (inCh && inCh === Number(currentVoiceViewChannelId)) {
      hideVoiceTextLocked();
      if (Number(currentChatId) !== Number(currentVoiceViewChannelId)) {
        await openChat(currentVoiceViewChannelId, channelName || 'Voice');
      }
    } else {
      showVoiceTextLocked(currentVoiceViewChannelId, channelName || 'Voice');
    }
  } catch (_) {
    showVoiceTextLocked(currentVoiceViewChannelId, channelName || 'Voice');
  }

  const nameEl = document.getElementById('voiceViewChannelName');
  if (nameEl) nameEl.textContent = (channelName || 'Voice').toString();

  const stateEl = document.getElementById('voiceViewState');
  try {
    const st = window.lbVoice?.getState?.();
    const inCh = Number(st?.channel_id || 0);
    if (stateEl) stateEl.textContent = inCh ? 'Подключено' : 'Не подключено';
  } catch (_) {
    if (stateEl) stateEl.textContent = 'Подключение...';
  }

  markVoiceSelectedInList(currentVoiceViewChannelId);
}

function closeVoiceView() {
  const voiceView = document.getElementById('voiceView');
  if (voiceView) voiceView.hidden = true;
  document.body.classList.remove('voice-view-open');
  document.body.classList.remove('voice-split-open');
  try { setVoiceDiscordLayout(false); } catch (_) {}
  // restore members panel in server mode
  try {
    const membersPanel = document.getElementById('membersPanel');
    if (membersPanel && currentServerId) membersPanel.hidden = false;
  } catch (_) {}
  currentVoiceViewChannelId = null;
  markVoiceSelectedInList(null);
}

// allow other modules (friends/settings/etc.) to reliably close/open voice view
window.openVoiceView = openVoiceView;
window.closeVoiceView = closeVoiceView;

document.addEventListener('lb:openVoiceView', (ev) => {
  const d = ev?.detail || {};
  openVoiceView(d.channel_id, d.channel_name);
});

// Voice module notifies when user leaves voice (so UI must not look "still in voice")
document.addEventListener('lb:voiceLeft', (ev) => {
  try {
    const ch = Number(ev?.detail?.channel_id || 0);
    const cur = Number(currentVoiceViewChannelId || 0);
    const wasVoiceChatOpen = Number(currentChatId || 0) === ch;

    if (!cur || !ch || cur === ch) {
      closeVoiceView();
    }

    // If we were looking at the voice text chat — return to last opened text chat for this server
    if (wasVoiceChatOpen && currentServerId) {
      const last = lastTextChatByServer.get(Number(currentServerId));
      const nextId = Number(last?.id || 0);
      if (nextId && nextId !== ch) {
        openChat(nextId, last?.name || chatNameById.get(nextId) || '');
      } else {
        // fallback: first non-voice channel
        try {
          const list = document.getElementById('channels-list');
          const first = list?.querySelector?.('.item.channel:not(.voice)')?.dataset?.channelId;
          const fid = first ? Number(first) : 0;
          if (fid && fid !== ch) openChat(fid, chatNameById.get(fid) || '');
        } catch (_) {}
      }
    }
  } catch (_) {
    try { closeVoiceView(); } catch (_) {}
  }
});

// Voice module notifies when user successfully joined a voice channel
document.addEventListener('lb:voiceJoined', (ev) => {
  try {
    const ch = Number(ev?.detail?.channel_id || 0);
    if (!ch) return;
    const name = (ev?.detail?.channel_name || chatNameById.get(ch) || 'Voice').toString();

    // Open voice view if it matches the currently selected voice channel
    if (Number(currentVoiceViewChannelId || 0) === ch) {
      hideVoiceTextLocked();
      openChat(ch, name);
    }
  } catch (_) {}
});


function setUiModeServer() {
    const channelsPanel = document.getElementById('channelsPanel');
    const channelsTitle = channelsPanel?.querySelector('.panelHeader h3');
    const dmList = document.getElementById('dmList');
    const channelsList = document.getElementById('channels-list');
    const membersPanel = document.getElementById('membersPanel');

    if (channelsTitle) channelsTitle.textContent = 'Каналы';
    channelsPanel?.classList.remove('dm-mode');

    if (dmList) dmList.hidden = true;
    if (channelsList) channelsList.hidden = false;
    if (membersPanel) membersPanel.hidden = false;

    try { updateChannelAdminUi(); } catch (_) {}
}

function setUiModeDm() {
    const channelsPanel = document.getElementById('channelsPanel');
    const channelsTitle = channelsPanel?.querySelector('.panelHeader h3');
    const dmList = document.getElementById('dmList');
    const channelsList = document.getElementById('channels-list');
    const membersPanel = document.getElementById('membersPanel');

    if (channelsTitle) channelsTitle.textContent = 'Чаты';
    channelsPanel?.classList.add('dm-mode');

    if (channelsList) channelsList.hidden = true;
    if (dmList) dmList.hidden = false;
    // On desktop keep members panel visible (voice members are shown there).
  if (membersPanel) membersPanel.hidden = (window.innerWidth <= 900);

    try { updateChannelAdminUi(); } catch (_) {}
}

const HIDDEN_DMS_KEY = 'lb:hidden_dm_chats_v1';
const HIDDEN_DMS_META_KEY = 'lb:hidden_dm_meta_v1';

let hiddenDmChats = new Set();
let hiddenDmMeta = new Map();
let dmMetaByChatId = new Map();

// DM ordering: keep local last-activity timestamps/ids to re-order instantly on new messages
const dmActivity = new Map(); // chatId -> { lastId?:number, at?:number }

function parseMaybeNumber(v) {
    const n = typeof v === 'string' ? Number(v) : Number(v);
    return Number.isFinite(n) ? n : null;
}

function extractFileNameFromMessageContent(text) {
    const s = (text || '').toString();
    // Robust parse (supports truncated previews from SQL substr and legacy/broken variants):
    //   [[file:ID|NAME|MIME|SIZE]]
    //   [[file:ID|NAME|MIME...  (truncated)
    //   [[file=ID|NAME|...     (legacy)
    //   [[file:ID]]NAME|MIME|SIZE]]   (broken legacy seen in DB)
    const i1 = s.indexOf('[[file:');
    const i2 = s.indexOf('[[file=');
    const i = i1 >= 0 ? i1 : i2;
    if (i < 0) return null;

    const tail = s.slice(i);

    // Canonical / legacy-with-pipe: "[[file:ID|NAME|..." or "[[file=ID|NAME|..."
    const headPipe = tail.match(/\[\[file[:=](\d+)\|/);
    if (headPipe) {
        // Position right after "[[file:ID|" (or "[[file=ID|")
        const nameStart = i + headPipe[0].length;
        if (nameStart >= s.length) return null;

        const rest = s.slice(nameStart);

        // Name ends at next "|" or closing "]]" or end (if truncated)
        let end = rest.indexOf('|');
        const endClose = rest.indexOf(']]');
        if (end === -1 || (endClose !== -1 && endClose < end)) end = endClose;
        if (end === -1) end = rest.length;

        let encName = rest.slice(0, end).trim();
        if (!encName) return null;

        // If truncated mid-name, strip trailing whitespace/ellipsis
        encName = encName.replace(/[\s\u2026]+$/g, '');

        try { return decodeURIComponent(encName); } catch (_) { return encName; }
    }

    // Broken legacy: "[[file:ID]]NAME|MIME|SIZE]]" (note: no "|" after ID)
    const headBroken = tail.match(/\[\[file:(\d+)\]\]/);
    if (!headBroken) return null;

    const nameStart = i + headBroken[0].length;
    if (nameStart >= s.length) return null;

    const rest = s.slice(nameStart);

    // Name ends at next "|" or closing "]]" or end
    let end = rest.indexOf('|');
    const endClose = rest.indexOf(']]');
    if (end === -1 || (endClose !== -1 && endClose < end)) end = endClose;
    if (end === -1) end = rest.length;

    let name = rest.slice(0, end).trim();
    if (!name) return null;

    name = name.replace(/[\s\u2026]+$/g, '');
    try { return decodeURIComponent(name); } catch (_) { return name; }
}

function extractAllFileNamesFromMessageContent(text) {
    const s = (text || '').toString();
    const out = [];
    if (!s.includes('[[file:') && !s.includes('[[file=')) return out;

    // canonical / legacy with pipe: [[file:ID|NAME|...]]
    try {
        const re = /\[\[file[:=](\d+)\|([^|\]]+)[^\]]*\]\]/g;
        let m;
        while ((m = re.exec(s)) !== null) {
            const enc = (m[2] || '').toString().trim();
            if (!enc) continue;
            let name = enc;
            try { name = decodeURIComponent(enc); } catch (_) {}
            out.push(name);
        }
    } catch (_) {}

    // broken legacy: [[file:ID]]NAME|...]]
    try {
        const re2 = /\[\[file:(\d+)\]\]([^|\]]+)[^\]]*\]\]/g;
        let m2;
        while ((m2 = re2.exec(s)) !== null) {
            const enc = (m2[2] || '').toString().trim();
            if (!enc) continue;
            let name = enc;
            try { name = decodeURIComponent(enc); } catch (_) {}
            out.push(name);
        }
    } catch (_) {}

    // unique, keep order
    const seen = new Set();
    const uniq = [];
    for (const n of out) {
        const k = (n || '').toString().trim();
        if (!k) continue;
        if (seen.has(k)) continue;
        seen.add(k);
        uniq.push(k);
    }
    return uniq;
}

function previewTextFromMessageContent(text) {
    const raw = (text || '').toString().trim();
    if (!raw) return '';
    const fn = extractFileNameFromMessageContent(raw);
    if (fn) return `📎 ${fn}`;

    if (raw.includes('[[file:') || raw.includes('[[file=')) {
        // hide internal file markers even if parsing failed
        const cleaned = raw
            // canonical / legacy with pipe
            .replace(/\[\[file[:=]\d+\|([^|\]]+)[^\]]*\]\]/g, (_, name) => {
                try { return `📎 ${decodeURIComponent(name)}`; } catch (_) { return `📎 ${name}`; }
            })
            // broken legacy: [[file:ID]]NAME|MIME|SIZE]]
            .replace(/\[\[file:(\d+)\]\]([^|\]]+)[^\]]*\]\]/g, (_, _id, name) => {
                try { return `📎 ${decodeURIComponent(name)}`; } catch (_) { return `📎 ${name}`; }
            })
            .replace(/\s+/g, ' ')
            .trim();
        return cleaned || '📎 вложение';
    }

    return raw.replace(/\s+/g, ' ').trim();
}


function dmPreviewFrom(dm) {
    const raw = (dm?.last_message_preview || '').toString().trim();
    if (raw) {
        // prevent leaking internal ids: show file name instead of [[file:id|...]]
        const fn = extractFileNameFromMessageContent(raw);
        if (fn) return fn;
        if (raw.includes('[[file:') || raw.includes('[[file=')) return '📎 вложение';
        // if raw contains file tag but parsing failed (no mime/size), still hide it
        const cleaned = raw
            .replace(/\[\[file:\d+\|([^|\]]+)[^\]]*\]\]/g, (_, name) => {
                try { return decodeURIComponent(name); } catch (_) { return name; }
            })
            .replace(/\s+/g, ' ')
            .trim();
        return cleaned;
    }

    // fallback: if server didn't provide preview but we have last message content-like text
    const fallback = (dm?.last_message_content || '').toString();
    const fn = extractFileNameFromMessageContent(fallback);
    if (fn) return fn;
    return '';
}

function dmSortKey(dm) {
    const chatId = parseMaybeNumber(dm?.chat_id);
    if (!chatId) return 0;
    const local = dmActivity.get(chatId);
    if (local?.lastId) return local.lastId;
    const fromApi = parseMaybeNumber(dm?.last_message_id) || parseMaybeNumber(dm?.last_message_msg_id);
    if (fromApi) return fromApi;
    const at = Date.parse(dm?.last_message_at || dm?.updated_at || '') || 0;
    return at;
}

function loadHiddenDmChats() {
    try {
        const raw = localStorage.getItem(HIDDEN_DMS_KEY) || '[]';
        const arr = JSON.parse(raw);
        if (Array.isArray(arr)) {
            hiddenDmChats = new Set(
                arr
                    .map((x) => Number(x))
                    .filter((n) => Number.isFinite(n) && n > 0)
            );
        }
    } catch (_) {
        hiddenDmChats = new Set();
    }
}

function saveHiddenDmChats() {
    try {
        localStorage.setItem(HIDDEN_DMS_KEY, JSON.stringify([...hiddenDmChats]));
    } catch (_) {}
}

function loadHiddenDmMeta() {
    try {
        const raw = localStorage.getItem(HIDDEN_DMS_META_KEY) || '{}';
        const obj = JSON.parse(raw);
        if (!obj || typeof obj !== 'object') return;
        hiddenDmMeta = new Map();
        for (const [k, v] of Object.entries(obj)) {
            const chatId = Number(k);
            if (!Number.isFinite(chatId) || chatId <= 0) continue;
            if (!v || typeof v !== 'object') continue;
            const otherId = Number(v.otherId);
            const otherName = (v.otherName || 'Unknown').toString();
            hiddenDmMeta.set(chatId, {
                otherId: Number.isFinite(otherId) ? otherId : 0,
                otherName,
            });
        }
    } catch (_) {
        hiddenDmMeta = new Map();
    }
}

function saveHiddenDmMeta() {
    try {
        const obj = {};
        for (const [chatId, meta] of hiddenDmMeta.entries()) {
            obj[String(chatId)] = {
                otherId: meta?.otherId ?? 0,
                otherName: (meta?.otherName || 'Unknown').toString(),
            };
        }
        localStorage.setItem(HIDDEN_DMS_META_KEY, JSON.stringify(obj));
    } catch (_) {}
}

function hideDmChat(chatId, meta) {
    const id = Number(chatId);
    if (!Number.isFinite(id) || id <= 0) return;

    hiddenDmChats.add(id);
    saveHiddenDmChats();

    if (meta && typeof meta === 'object') {
        hiddenDmMeta.set(id, {
            otherId: Number(meta.otherId) || 0,
            otherName: (meta.otherName || 'Unknown').toString(),
        });
        saveHiddenDmMeta();
    }

    loadDmList().catch((e) => console.warn('[UI] loadDmList after hide failed', e));
}

function maybeUnhideDmOnIncoming(chatId) {
    const id = Number(chatId);
    if (!Number.isFinite(id) || id <= 0) return;
    if (!hiddenDmChats.has(id)) return;

    hiddenDmChats.delete(id);
    saveHiddenDmChats();

    const meta = hiddenDmMeta.get(id) || dmMetaByChatId.get(id);
    const otherName = (meta?.otherName || chatNameById.get(id) || `Чат #${id}`).toString();

    // refresh list (не переключаем чат автоматически)
    (async () => {
        try { await loadDmList(); } catch (_) {}
    })();
}

function bumpListItemToTop(listEl, itemEl) {
    try {
        if (!listEl || !itemEl) return;
        if (listEl.firstChild === itemEl) return;
        listEl.insertBefore(itemEl, listEl.firstChild);
    } catch (_) {}
}

function updateDmListItemOnMessage(chatId, previewText) {
    const dmList = document.getElementById('dmList');
    if (!dmList) return;
    const el = dmList.querySelector(`.item.dm[data-chat-id="${chatId}"]`);
    if (!el) return;
    const sub = el.querySelector('.sub');
    if (sub && previewText) sub.textContent = previewText;
    bumpListItemToTop(dmList, el);
}

function updateChannelListItemOnMessage(chatId, unreadCount) {
    const list = document.getElementById('channels-list');
    if (!list) return;
    const el = list.querySelector(`.item.channel[data-channel-id="${chatId}"]`);
    if (!el) return;
    // update badge
    const badge = el.querySelector('.badge-unread');
    const n = Number(unreadCount);
    const show = Number.isFinite(n) && n > 0;
    if (show) {
        if (badge) badge.textContent = n > 99 ? '99+' : String(n);
        else {
            const title = el.querySelector('.title');
            if (title) {
                const span = document.createElement('span');
                span.className = 'badge-unread';
                span.title = 'Непрочитано';
                span.textContent = n > 99 ? '99+' : String(n);
                title.appendChild(span);
            }
        }
    } else {
        badge?.remove?.();
    }
    // do NOT bump channel ordering here (server channels usually fixed)
}

loadHiddenDmChats();
loadHiddenDmMeta();

async function loadDmList() {
    const dmList = document.getElementById('dmList');
    if (!dmList) return;

    try {
        const dms = await api('/api/dms');
        renderDmList(dms);
    } catch (e) {
        console.warn('[UI] Failed to load DM list', e);
        dmList.innerHTML = `<div class="muted" style="padding:12px;">Не удалось загрузить чаты</div>`;
    }
}

function renderDmList(dms) {
    const dmList = document.getElementById('dmList');
    if (!dmList) return;

    dmList.innerHTML = '';

    if (!Array.isArray(dms) || dms.length === 0) {
        dmList.innerHTML = `<div class="muted" style="padding:12px;">Нет личных сообщений</div>`;
        return;
    }

    const sorted = [...dms].sort((a, b) => {
        const ka = dmSortKey(a);
        const kb = dmSortKey(b);
        // desc
        return (kb > ka) ? 1 : (kb < ka) ? -1 : 0;
    });

    for (const dm of sorted) {
        const item = document.createElement('div');
        const chatId = Number(dm?.chat_id);
        const otherId = Number(dm?.other_user_id);
        const otherName = (dm?.other_username || 'Unknown').toString();
        const preview = dmPreviewFrom(dm);

        // cache activity from API
        const apiLastId = parseMaybeNumber(dm?.last_message_id) || parseMaybeNumber(dm?.last_message_msg_id);
        const apiAt = Date.parse(dm?.last_message_at || dm?.updated_at || '') || 0;
        if (Number.isFinite(chatId) && chatId > 0) {
            const cur = dmActivity.get(chatId) || {};
            dmActivity.set(chatId, {
                lastId: apiLastId || cur.lastId || 0,
                at: apiAt || cur.at || 0,
            });
        }

        if (Number.isFinite(chatId) && chatId > 0) {
            dmMetaByChatId.set(chatId, { otherId, otherName });
        }

        if (hiddenDmChats.has(chatId)) {
            // keep meta for auto-unhide
            if (Number.isFinite(chatId) && chatId > 0) {
                hiddenDmMeta.set(chatId, { otherId, otherName });
                saveHiddenDmMeta();
            }
            continue;
        }

        item.className = `item dm ${(!currentServerId && currentChatId === chatId) ? 'active' : ''}`;
        item.dataset.chatId = String(chatId);
        item.dataset.otherUserId = String(otherId);

        const letter = (otherName.charAt(0) || 'U').toUpperCase();

        item.innerHTML = `
            <div class="avatar">${escapeHtml(letter)}</div>
            <div class="text">
                <div class="title">${escapeHtml(otherName)}</div>
                <div class="sub">${escapeHtml(preview || 'Личное сообщение')}</div>
            </div>
            <button class="dm-hide" type="button" title="Скрыть чат">✕</button>
        `;

        item.querySelector('.dm-hide')?.addEventListener('click', (e) => {
            e.preventDefault();
            e.stopPropagation();
            hideDmChat(chatId, { otherId, otherName });
        });

        item.addEventListener('click', () => {
            openDmChat(chatId, otherName).catch((e) => console.warn('[UI] openDmChat failed', e));
        });

        dmList.appendChild(item);
    }
}

async function openDmChat(chatId, otherName) {
    currentServerId = null;

    // If user is in Friends view, chat UI is hidden. Ensure we exit Friends view.
    if (typeof window.closeFriends === 'function') {
        try { window.closeFriends(); } catch (e) { console.warn('[UI] closeFriends failed', e); }
    }

    setUiModeDm();
    await loadDmList(); // refresh active state + order
    await openChat(chatId, otherName);
}

window.lbLoadDmList = loadDmList;

window.addEventListener('laberry:dm-open', (ev) => {
    const d = ev?.detail || {};
    const chatId = Number(d.chatId);
    const username = (d.username || 'Unknown').toString();
    if (!Number.isFinite(chatId) || chatId <= 0) return;
    openDmChat(chatId, username).catch((e) => console.warn('[UI] dm-open failed', e));
});


async function openChat(chatId, title) {
    // If voice view is open in split mode for the same channel, do not close it.
    // Otherwise, switching chats should close voice layout.
    try {
        const cid = Number(chatId);
        const vid = Number(currentVoiceViewChannelId || 0);
        if (!vid || cid !== vid) closeVoiceView();
    } catch (_) {}
    try {
        const chatView = document.getElementById('chatView');
        const friendsView = document.getElementById('friendsView');
        if (chatView) chatView.hidden = false;
        if (friendsView) friendsView.hidden = true;
    } catch (_) {}
    console.log(`[UI] Opening chat ${chatId} (${title})`);
    const seq = ++openChatSeq;

    // unlock composer if it was locked by voice
    try { hideVoiceTextLocked(); } catch (_) {}
    try {
        const composer = document.getElementById('composer');
        if (composer) composer.hidden = false;
    } catch (_) {}
    
    currentChatId = chatId;
    // prevent cross-chat reply leaks
    clearReplyTo();
    try { chatNameById.set(chatId, title || `#${chatId}`); } catch (_) {}
    sessionStorage.setItem("lastChatId", chatId.toString());

    // remember last opened TEXT (non-voice) chat for this server
    try {
        const kind = (chatKindById.get(chatId) || 'text').toString();
        if (currentServerId && kind !== 'voice') {
            lastTextChatByServer.set(Number(currentServerId), { id: Number(chatId), name: (title || '').toString() });
        }
    } catch (_) {}
    
    const chatTitleElement = $("chat-title");
    if (chatTitleElement) {
        const isDm = !currentServerId;
        chatTitleElement.textContent = isDm ? `@ ${title}` : `# ${title}`;
    }

    const dmCallBtn = document.getElementById('dmCallBtn');
    if (dmCallBtn) {
        const isDm = !currentServerId;
        dmCallBtn.hidden = !isDm;
    }
    
    document.querySelectorAll('.item.channel').forEach(item => {
        const itemId = parseInt(item.dataset.channelId);
        item.classList.toggle('active', itemId === chatId);
    });


    document.querySelectorAll('#dmList .item.dm').forEach(item => {
        const itemId = parseInt(item.dataset.chatId);
        item.classList.toggle('active', !currentServerId && itemId === chatId);
    });
    
    if (window.innerWidth <= 900 || isTouchUi()) {
        hideChannelsMenu();
        hideServersMenu();
        hideMembersMenu();
    }
    
    const messagesContainer = $("messages");
    if (messagesContainer) {
        messagesContainer.innerHTML = '';
    }

    chatPaging = { chatId, minId: null, hasMore: true, loading: false };
    
    try {
        const msgsUrl = currentServerId
            ? `/api/servers/${currentServerId}/chats/${chatId}/messages?limit=${MESSAGES_PAGE_SIZE}`
            : `/api/dms/${chatId}/messages?limit=${MESSAGES_PAGE_SIZE}`;

        const msgs = await api(msgsUrl);

        if (seq !== openChatSeq) {
            console.log(`[UI] openChat(${chatId}) stale response ignored (seq=${seq}, current=${openChatSeq})`);
            return;
        }
        
        if (Array.isArray(msgs) && msgs.length > 0) {
            // msgs приходят уже по возрастанию id
            msgs.forEach(m => addMessage(m, { dedup: false, history: true }));

            chatPaging.minId = msgs[0]?.id ?? null;
            chatPaging.hasMore = msgs.length >= MESSAGES_PAGE_SIZE;

            // scroll logic:
            // - no unread -> start at bottom (and stick to bottom while media loads)
            // - has unread -> jump to last seen message + marker
            if (messagesContainer) {
                wireMessagesAutoScroll();

                const latestId = msgs[msgs.length - 1]?.id ?? null;
                const lastSeen = currentServerId ? getLastSeenId(currentServerId, chatId) : null;

                if (lastSeen !== null && latestId !== null && Number(latestId) > Number(lastSeen)) {
                    const unread = msgs.filter(x => (x?.id ?? 0) > Number(lastSeen)).length || (currentServerId ? (getUnreadCount(currentServerId, chatId) || 1) : 1);
                    if (currentServerId) setUnreadCount(currentServerId, chatId, unread);
                    setStickToBottom(false);
                    insertNewMarkerAfter(messagesContainer, lastSeen);
                    scrollToMessageId(messagesContainer, lastSeen);
                } else {
                    removeNewMarker(messagesContainer);
                    if (currentServerId) {
                        clearUnreadCount(currentServerId, chatId);
                        updateChannelListItemOnMessage(chatId, 0);
                        if (latestId !== null) setLastSeenId(currentServerId, chatId, latestId);
                    }
                    setStickToBottom(true);
                    scrollToBottomSafe(messagesContainer, 6);
                }

                updateJumpBtn();
            }
        } else {
            const emptyMsg = document.createElement('div');
            emptyMsg.className = 'empty-chat';
            const isDm = !currentServerId;
            emptyMsg.innerHTML = isDm
                ? `
                <h3>Диалог с ${escapeHtml(title)} 👋</h3>
                <p>Это начало переписки. Напишите первое сообщение!</p>
            `
                : `
                <h3>Добро пожаловать в #${escapeHtml(title)}! 👋</h3>
                <p>Это начало канала #${escapeHtml(title)}. Напишите первое сообщение!</p>
            `;
            if (messagesContainer) {
                messagesContainer.appendChild(emptyMsg);
                wireMessagesAutoScroll();
                removeNewMarker(messagesContainer);
                clearUnreadCount(currentServerId, chatId);
                        updateChannelListItemOnMessage(chatId, 0);
                setStickToBottom(true);
                scrollToBottomSafe(messagesContainer, 3);
                updateJumpBtn();
            }
        }
        
        // Join room via WS. If WS isn't connected yet, websocket-manager will queue the message.
        if (wsManager && typeof wsManager.joinRoom === 'function') {
            wsManager.joinRoom(chatId);
        }
    } catch (e) {
        console.error("[UI] Failed to load messages", e);
    }
}

function setupMessagesInfiniteScroll() {
    const container = $("messages");
    if (!container) return;

    const empty = container.querySelector('.empty-chat');
    if (empty) empty.remove();

    if (container.dataset.infiniteScroll === '1') return;
    container.dataset.infiniteScroll = '1';

    container.addEventListener('scroll', () => {
        if (!currentChatId) return;
        if (!chatPaging || chatPaging.chatId !== currentChatId) return;
        if (chatPaging.loading || !chatPaging.hasMore || !chatPaging.minId) return;

        if (container.scrollTop > 160) return;

        loadOlderMessages().catch((e) => console.warn('[UI] loadOlderMessages failed', e));
    });
}

async function loadOlderMessages() {
    const container = $("messages");
    if (!container) return;
    if (!currentChatId) return;
    if (!chatPaging || chatPaging.chatId !== currentChatId) return;
    if (chatPaging.loading || !chatPaging.hasMore || !chatPaging.minId) return;

    chatPaging.loading = true;

    const beforeId = chatPaging.minId;
    const oldHeight = container.scrollHeight;
    const oldTop = container.scrollTop;

    const url = currentServerId
        ? `/api/servers/${currentServerId}/chats/${currentChatId}/messages?limit=${MESSAGES_PAGE_SIZE}&before_id=${beforeId}`
        : `/api/dms/${currentChatId}/messages?limit=${MESSAGES_PAGE_SIZE}&before_id=${beforeId}`;

    const msgs = await api(url);
    if (!Array.isArray(msgs) || msgs.length === 0) {
        chatPaging.hasMore = false;
        chatPaging.loading = false;
        return;
    }

    // prepend in correct order
    for (let i = msgs.length - 1; i >= 0; i--) {
        addMessage(msgs[i], { dedup: false, prepend: true });
    }

    chatPaging.minId = msgs[0]?.id ?? chatPaging.minId;
    if (msgs.length < MESSAGES_PAGE_SIZE) chatPaging.hasMore = false;

    const newHeight = container.scrollHeight;
    container.scrollTop = (newHeight - oldHeight) + oldTop;

    chatPaging.loading = false;
}

function setupWebSocketHandlers() {
    window.onChatMessage = (data) => {
        console.log('[APP] WebSocket message received:', data);
        if (!data) return;

        // support service events (typing/upload) if server sends them
        if (data.type === 'typing_state' || data.type === 'typing' || data.type === 'upload_state') {
            try { window.onWsMessage?.(data); } catch (_) {}
            return;
        }

        // message deleted
        if (data.type === 'message_deleted') {
            const roomIdRaw = data.room_id;
            if (roomIdRaw === undefined || roomIdRaw === null) return;
            const roomId = typeof roomIdRaw === 'string' ? parseInt(roomIdRaw, 10) : roomIdRaw;
            if (!Number.isFinite(roomId)) return;
            const mid = parseMaybeNumber(data.id) || parseMaybeNumber(data.message_id);
            if (!mid) return;
            if (roomId === currentChatId) {
                const el = document.querySelector(`.message[data-msg-id="${mid}"]`);
                try { el && el.remove(); } catch (_) {}
            }
            return;
        }

        // reactions update
        if (data.type === 'reaction') {
            if (data.room_id === undefined || data.room_id === null) return;
            const roomId = typeof data.room_id === 'string' ? parseInt(data.room_id, 10) : data.room_id;
            if (!Number.isFinite(roomId)) return;
            const mid = parseMaybeNumber(data.message_id);
            if (roomId === currentChatId && mid) {
                refreshMessageReactions(mid, { force: true });
            }
            return;
        }

        if (data.room_id === undefined || data.room_id === null) return;

        const roomId = typeof data.room_id === 'string' ? parseInt(data.room_id, 10) : data.room_id;
        if (!Number.isFinite(roomId)) return;

        const sender = (data.sender_username || data.sender_id || 'Unknown').toString();
        const content = (data.content || '').toString();
        const msgId = parseMaybeNumber(data.id) || 0;
        const replyToId = parseMaybeNumber(data.reply_to_id);
        const replyPreview = (data.reply_preview && typeof data.reply_preview === 'object') ? data.reply_preview : null;
        const senderAvatar = parseMaybeNumber(data.sender_avatar_file_id);
        const reactions = Array.isArray(data.reactions) ? data.reactions : null;

        // ignore echo from yourself
        const myName = (currentUser?.username || currentUser?.nickname || '').toString();
        if (myName && sender === myName) {
            // still append if it's current chat and missing
            if (roomId === currentChatId) {
                addMessage({
                    id: data.id,
                    chat_id: roomId,
                    sender_id: data.sender_id,
                    sender_username: sender,
                    sender_avatar_file_id: senderAvatar,
                    content,
                    timestamp: data.timestamp,
                    reply_to_id: replyToId,
                    reply_preview: replyPreview,
                    reactions: reactions || undefined,
                });
            }
            return;
        }

        // DM list ordering + preview
        const dmPreview = (() => {
            const fn = extractFileNameFromMessageContent(content);
            if (fn) return fn;
            if (content.includes('[[file:') || content.includes('[[file=')) return '📎 вложение';
            const c = content.trim();
            if (!c) return '📎 вложение';
            return c.replace(/\s+/g, ' ').slice(0, 120);
        })();

        if (dmMetaByChatId.has(roomId) || hiddenDmMeta.has(roomId)) {
            // unhide on incoming
            maybeUnhideDmOnIncoming(roomId);
            dmActivity.set(roomId, { lastId: msgId || (dmActivity.get(roomId)?.lastId || 0), at: Date.now() });
            updateDmListItemOnMessage(roomId, dmPreview);
        }

        // server channel unread updates
        if (currentServerId && roomId !== currentChatId) {
            const kind = (chatKindById.get(roomId) || 'text').toString();
            if (kind !== 'voice') {
                incUnreadCount(currentServerId, roomId, 1);
            const n = getUnreadCount(currentServerId, roomId);
            updateChannelListItemOnMessage(roomId, n);
            }
        }

        // append to current chat
        if (roomId === currentChatId) {
            addMessage({
                id: data.id,
                chat_id: roomId,
                sender_id: data.sender_id,
                sender_username: sender,
                sender_avatar_file_id: senderAvatar,
                content,
                timestamp: data.timestamp,
                reply_to_id: replyToId,
                reply_preview: replyPreview,
                reactions: reactions || undefined,
            });
            // mark read
            if (currentServerId) {
                clearUnreadCount(currentServerId, roomId);
                updateChannelListItemOnMessage(roomId, 0);
            }
        }

        // notifications for any subscribed chats (ws joins accessible rooms)
        if ((chatKindById.get(roomId) || 'text') !== 'voice') {
            notifyForIncomingMessage(roomId, sender, content);
        }
    };
}

// ===== Typing / Upload indicator (above composer) =====
// roomId -> Map(username -> { kind:'typing'|'upload', activity:'text'|'image'|'video'|'file', at:number })
const roomTyping = new Map();

function ensureTypingIndicatorEl() {
    let el = document.getElementById('typingIndicator');
    if (el) return el;
    const composer = document.getElementById('composer');
    if (!composer) return null;
    el = document.createElement('div');
    el.id = 'typingIndicator';
    el.className = 'typing-indicator';
    el.hidden = true;
    composer.parentNode.insertBefore(el, composer);
    return el;
}

function _activityLabel(kind, activity) {
    const a = (activity || '').toString().toLowerCase();
    if (kind === 'upload') {
        if (a === 'image' || a === 'photo' || a === 'img') return 'отправляет фото…';
        if (a === 'video') return 'отправляет видео…';
        return 'отправляет файл…';
    }
    // typing
    if (a === 'image' || a === 'video' || a === 'file') return _activityLabel('upload', a);
    return 'печатает…';
}

function refreshTypingIndicator(roomId) {
    if (Number(roomId) !== Number(currentChatId)) return;
    const el = ensureTypingIndicatorEl();
    if (!el) return;

    const room = roomTyping.get(Number(roomId));
    if (!room || room.size === 0) {
        el.hidden = true;
        el.textContent = '';
        return;
    }

    const entries = Array.from(room.entries()).map(([username, st]) => ({ username, ...(st || {}) }));
    const uploads = entries.filter(e => e.kind === 'upload');
    const typings = entries.filter(e => e.kind !== 'upload');

    const list = uploads.length ? uploads : typings;
    if (!list.length) {
        el.hidden = true;
        el.textContent = '';
        return;
    }

    const first = list[0];
    const others = list.length - 1;
    const label = _activityLabel(first.kind, first.activity);

    el.hidden = false;
    el.textContent = others > 0 ? `${first.username} и ещё ${others} ${label}` : `${first.username} ${label}`;
}

window.onWsMessage = (msg) => {
    try {
        if (!msg) return;

        const myName = (currentUser?.username || currentUser?.nickname || '').toString();

        // old format: {type:'typing_state', data:{room_id, username, is_typing}}
        if (msg.type === 'typing_state') {
            const d = msg.data || msg;
            const roomId = Number(d.room_id);
            const username = (d.username || '').toString();
            const isTyping = Boolean(d.is_typing);
            if (!Number.isFinite(roomId) || !username) return;
            if (myName && username === myName) return;

            let room = roomTyping.get(roomId);
            if (!room) {
                room = new Map();
                roomTyping.set(roomId, room);
            }

            if (!isTyping) {
                room.delete(username);
                if (room.size === 0) roomTyping.delete(roomId);
            } else {
                room.set(username, { kind: 'typing', activity: 'text', at: Date.now() });
            }
            refreshTypingIndicator(roomId);
            return;
        }

        // new format from server ws/chat.rs:
        // {type:'typing'|'upload_state', chat_id, username, state:'start'|'stop', activity:'text'|'image'|'video'|'file'}
        if (msg.type === 'typing' || msg.type === 'upload_state') {
            const roomId = Number(msg.chat_id);
            const username = (msg.username || '').toString();
            const state = (msg.state || 'start').toString().toLowerCase();
            const activity = (msg.activity || 'text').toString().toLowerCase();
            if (!Number.isFinite(roomId) || !username) return;
            if (myName && username === myName) return;

            let room = roomTyping.get(roomId);
            if (!room) {
                room = new Map();
                roomTyping.set(roomId, room);
            }

            if (state === 'stop' || state === 'end' || state === '0') {
                room.delete(username);
                if (room.size === 0) roomTyping.delete(roomId);
            } else {
                const kind = (msg.type === 'upload_state' || (activity && activity !== 'text')) ? 'upload' : 'typing';
                room.set(username, { kind, activity: activity || 'text', at: Date.now() });
            }

            refreshTypingIndicator(roomId);
            return;
        }

        if (msg.type === 'user_online' || msg.type === 'user_offline') {
            if (currentServerId) {
                loadMembers(currentServerId).catch(() => {});
            }
            refreshFriendsStatus().catch(() => {});
            return;
        }
    } catch (e) {
        console.warn('[WS] typing parse failed', e);
    }
};

setInterval(() => {
    const now = Date.now();
    for (const [roomId, room] of roomTyping.entries()) {
        for (const [name, st] of room.entries()) {
            const at = Number(st?.at || 0);
            if (!at || (now - at) > 3500) room.delete(name);
        }
        if (room.size === 0) roomTyping.delete(roomId);
        refreshTypingIndicator(roomId);
    }
}, 1000);

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

                // После WS-коннекта presence в БД обновлён — перезагрузим список участников
                if (currentServerId) {
                    loadMembers(currentServerId).catch(e => console.warn('[APP] loadMembers after WS connect failed', e));
                }

                // Лёгкий поллинг, чтобы онлайн/оффлайн не зависали (без WS-событий presence)
                if (!membersPollTimer) {
                    membersPollTimer = setInterval(() => {
                        if (currentServerId) loadMembers(currentServerId).catch(() => {});
                        refreshFriendsStatus().catch(() => {});
                    }, 10000);
                }
                
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

function setupMessageComposer() {
    const composerForm = document.getElementById('composer');
    if (!composerForm) {
        console.error('[APP] Composer form not found!');
        return;
    }

    // remove old listeners (hot reload safe)
    const newForm = composerForm.cloneNode(true);
    composerForm.parentNode.replaceChild(newForm, composerForm);

    const form = document.getElementById('composer');
    const input = document.getElementById('message');
    const attachBtn = document.getElementById('attachBtn');
    const fileInput = document.getElementById('fileInput');
    const attachmentsEl = document.getElementById('attachments');
    const sendBtn = document.getElementById('sendBtn');

    let isSubmitting = false;
    let pending = [];
    let pendingSeq = 0;

    const renderPending = () => {
        if (!attachmentsEl) return;

        attachmentsEl.innerHTML = '';
        if (!pending.length) {
            attachmentsEl.hidden = true;
            return;
        }

        attachmentsEl.hidden = false;

        for (const it of pending) {
            const chip = document.createElement('div');
            chip.className = 'attachment-chip';
            chip.innerHTML = `
              <span class="name">${escapeHtml(it.name)}</span>
              <span class="meta">${escapeHtml(formatBytes(it.file?.size))}</span>
              <button class="remove" type="button" title="Убрать">✕</button>
            `;

            chip.querySelector('button.remove')?.addEventListener('click', () => {
                pending = pending.filter(x => x.key !== it.key);
                renderPending();
            });

            attachmentsEl.appendChild(chip);
        }
    };

    const addFiles = (files) => {
        if (!files) return;

        const list = Array.from(files);
        let added = 0;

        for (let f of list) {
            if (!f) continue;

            // some clipboard images have empty name
            let name = (f.name || '').toString().trim();
            if (!name) {
                const ext = (f.type && f.type.includes('/')) ? f.type.split('/')[1] : 'png';
                name = `pasted_${Date.now()}_${Math.floor(Math.random() * 1000)}.${ext}`;
                try {
                    f = new File([f], name, { type: f.type || 'application/octet-stream' });
                } catch (_) {
                    // ignore
                }
            }

            // 50MB server limit
            if ((f.size || 0) > 50 * 1024 * 1024) {
                alert(`Файл слишком большой (лимит 50MB): ${name}`);
                continue;
            }

            pending.push({
                key: `f_${++pendingSeq}`,
                file: f,
                name,
                mime: (f.type || 'application/octet-stream').toLowerCase(),
                size: f.size || 0,
            });
            added++;
        }

        if (added) renderPending();
    };

    attachBtn?.addEventListener('click', () => {
        if (!fileInput) return;
        // iOS Safari может игнорировать programmatic click, если инпут был hidden/display:none.
        // Если attachBtn это <label for="fileInput"> — нативный клик уже откроет пикер.
        if ((attachBtn?.tagName || '').toUpperCase() === 'LABEL') return;
        fileInput.click();
    });

    fileInput?.addEventListener('change', () => {
        addFiles(fileInput.files);
        fileInput.value = '';
    });

    input?.addEventListener('paste', (e) => {
        const cd = e.clipboardData;
        if (!cd || !cd.items) return;

        const imgs = [];
        for (const item of cd.items) {
            if (item && item.kind === 'file' && item.type && item.type.startsWith('image/')) {
                const f = item.getAsFile();
                if (f) imgs.push(f);
            }
        }

        if (imgs.length) {
            e.preventDefault();
            addFiles(imgs);
        }
    });

    const sendMessage = async (content, opts = {}) => {
        const c = (content || '').toString();
        const url = currentServerId
            ? `/api/servers/${currentServerId}/chats/${currentChatId}/messages`
            : `/api/dms/${currentChatId}/messages`;

        const replyId = (opts && opts.replyId !== undefined) ? opts.replyId : replyToMessageId;
        const payload = await api(url, {
            method: 'POST',
            body: JSON.stringify({ content: c, reply_to_id: replyId || null })
        });

        const msg = {
            id: payload?.id,
            chat_id: currentChatId,
            sender_username: currentUser?.username || 'Вы',
            content: c,
            timestamp: payload?.timestamp || Date.now(),
        };

        // re-order DM list immediately after send
        if (!currentServerId && Number.isFinite(Number(currentChatId))) {
            const chatId = Number(currentChatId);
            const prev = extractFileNameFromMessageContent(c) || ((c.includes('[[file:') || c.includes('[[file=')) ? '📎 вложение' : (c.replace(/\s+/g, ' ').trim().slice(0, 120))) || '📎 вложение';
            dmActivity.set(chatId, { lastId: parseMaybeNumber(msg.id) || (dmActivity.get(chatId)?.lastId || 0), at: Date.now() });
            updateDmListItemOnMessage(chatId, prev);
        }
        const rp = (opts && opts.replyPreview) ? opts.replyPreview : replyToPreview;
        if (replyId && rp) {
            msg.reply_preview = {
                id: replyId,
                sender_username: rp.sender || rp.sender_username || '',
                content: rp.text || rp.content || '',
            };
            msg.reply_to_id = replyId;
        }

        // reset reply draft only for the active composer (not for background upload jobs)
        if (replyId && (!opts || opts.replyId === undefined)) {
            replyToMessageId = null;
            replyToPreview = null;
            if (replyBarEl) replyBarEl.hidden = true;
        }

        const id = msg.id;
        if (id !== null && id !== undefined && wsManager && wsManager.isConnected) {
            setTimeout(() => {
                if (msg.chat_id !== currentChatId) return;
                if (!hasSeen(msg.chat_id, id)) {
                    addMessage(msg);
                }
            }, 600);
        } else {
            addMessage(msg);
        }

        // mark read for current server chat
        if (currentServerId) {
            clearUnreadCount(currentServerId, currentChatId);
            updateChannelListItemOnMessage(currentChatId, 0);
        }
    };

    // ===== Upload queue (progress + cancel) =====
    const uploadQueueEl = document.getElementById('uploadQueue');
    let uploadSeq = 1;
    const uploadJobs = []; // {jobId, chatId, serverId, text, replyId, replyPreview, files:[{file, name, mime, size, fileId}], status, loadedBytes, totalBytes, xhrs:[], err?:string}

    const guessUploadActivity = (filesArr) => {
        const items = Array.isArray(filesArr) ? filesArr : [];
        const hasVideo = items.some(it => (it?.mime || it?.file?.type || '').toString().toLowerCase().startsWith('video/'));
        if (hasVideo) return 'video';
        const hasImg = items.some(it => (it?.mime || it?.file?.type || '').toString().toLowerCase().startsWith('image/'));
        if (hasImg) return 'image';
        return 'file';
    };

    const wsSendState = (typ, state, activity) => {
        try {
            if (!wsManager || !wsManager.isConnected || typeof wsManager.send !== 'function') return;
            if (!currentChatId) return;
            wsManager.send({
                type: typ,
                data: {
                    chat_id: Number(currentChatId),
                    state: state,
                    activity: activity || 'text'
                }
            });
        } catch (_) {}
    };

    const renderUploadQueue = () => {
        if (!uploadQueueEl) return;
        const cid = Number(currentChatId);
        const jobs = uploadJobs.filter(j => Number(j.chatId) === cid);

        if (!jobs.length) {
            uploadQueueEl.hidden = true;
            uploadQueueEl.innerHTML = '';
            return;
        }

        uploadQueueEl.hidden = false;
        uploadQueueEl.innerHTML = jobs.map(j => {
            const total = Number(j.totalBytes || 0);
            const loaded = Number(j.loadedBytes || 0);
            const pct = total > 0 ? Math.max(0, Math.min(100, Math.round((loaded / total) * 100))) : 0;
            const name = j.files.length === 1
                ? (j.files[0]?.name || j.files[0]?.file?.name || 'file')
                : `${j.files.length} файлов`;

            const sub = j.status === 'uploading'
                ? `${pct}% • ${formatBytes(loaded)} / ${formatBytes(total)}`
                : (j.status === 'sending' ? 'Отправка сообщения…' : (j.status === 'failed' ? (j.err || 'Ошибка') : (j.status === 'canceled' ? 'Отменено' : '')));

            return `
              <div class="upload-job" data-job="${j.jobId}">
                <div class="u-left">
                  <div class="u-name">${escapeHtml(name)}</div>
                  <div class="u-sub">${escapeHtml(sub)}</div>
                  <div class="u-bar"><i style="width:${pct}%"></i></div>
                </div>
                <div class="u-actions">
                  ${j.status === 'uploading' || j.status === 'sending'
                    ? `<button class="u-btn" type="button" data-act="cancel">Отмена</button>`
                    : `<button class="u-btn" type="button" data-act="close">✕</button>`}
                </div>
              </div>
            `;
        }).join('');

        uploadQueueEl.querySelectorAll('button[data-act="cancel"]').forEach(btn => {
            btn.addEventListener('click', (e) => {
                const row = e.target?.closest?.('[data-job]');
                const jobId = Number(row?.getAttribute('data-job'));
                if (!Number.isFinite(jobId)) return;
                cancelUploadJob(jobId);
            });
        });

        uploadQueueEl.querySelectorAll('button[data-act="close"]').forEach(btn => {
            btn.addEventListener('click', (e) => {
                const row = e.target?.closest?.('[data-job]');
                const jobId = Number(row?.getAttribute('data-job'));
                if (!Number.isFinite(jobId)) return;
                const idx = uploadJobs.findIndex(j => j.jobId === jobId);
                if (idx >= 0) uploadJobs.splice(idx, 1);
                renderUploadQueue();
            });
        });
    };

    const uploadFileXHR = (file, chatId, onProgress, attachXhr) => {
        return new Promise((resolve, reject) => {
            const xhr = new XMLHttpRequest();
            xhr.open('POST', '/api/files', true);
            xhr.responseType = 'json';

            const token = localStorage.getItem('auth_token');
            if (token) {
                try { xhr.setRequestHeader('Authorization', `Bearer ${token}`); } catch (_) {}
            }

            xhr.upload.onprogress = (e) => {
                try {
                    if (!onProgress) return;
                    if (e && e.lengthComputable) onProgress(e.loaded, e.total);
                    else onProgress(e.loaded || 0, null);
                } catch (_) {}
            };

            xhr.onload = () => {
                const ok = xhr.status >= 200 && xhr.status < 300;
                if (!ok) {
                    reject(new Error(`upload_failed:${xhr.status}`));
                    return;
                }
                const resp = xhr.response || (() => {
                    try { return JSON.parse(xhr.responseText || '{}'); } catch (_) { return null; }
                })();
                resolve(resp);
            };

            xhr.onerror = () => reject(new Error('upload_network_error'));
            xhr.onabort = () => reject(new Error('upload_aborted'));

            const fd = new FormData();
            fd.append('chat_id', String(chatId));
            fd.append('file', file, file.name || 'file');

            if (typeof attachXhr === 'function') attachXhr(xhr);
            xhr.send(fd);
        });
    };

    const cancelUploadJob = (jobId) => {
        const job = uploadJobs.find(j => j.jobId === jobId);
        if (!job) return;
        job.status = 'canceled';
        try {
            for (const x of (job.xhrs || [])) {
                try { x.abort(); } catch (_) {}
            }
        } catch (_) {}
        wsSendState('upload_state', 'stop', job.activity);
        renderUploadQueue();
    };

    const startUploadJob = async (job) => {
        job.status = 'uploading';
        job.loadedBytes = 0;
        job.totalBytes = job.files.reduce((a, it) => a + (Number(it.size || it.file?.size || 0) || 0), 0);
        job.xhrs = [];
        renderUploadQueue();

        wsSendState('upload_state', 'start', job.activity);

        let doneBytes = 0;
        try {
            for (const it of job.files) {
                if (job.status === 'canceled') throw new Error('canceled');

                const f = it.file;
                if (!f) continue;

                const res = await uploadFileXHR(f, job.chatId, (loaded, total) => {
                    const curTotal = Number(total || it.size || f.size || 0);
                    const curLoaded = Number(loaded || 0);
                    // Progress is: doneBytes + current file loaded
                    job.loadedBytes = Math.min(job.totalBytes, doneBytes + Math.min(curLoaded, curTotal || curLoaded));
                    renderUploadQueue();
                }, (xhr) => {
                    job.xhrs.push(xhr);
                });

                const id = res?.id;
                if (!id) throw new Error('upload_failed');
                it.fileId = id;

                doneBytes += Number(it.size || f.size || 0);
                job.loadedBytes = Math.min(job.totalBytes, doneBytes);
                renderUploadQueue();
            }

            if (job.status === 'canceled') throw new Error('canceled');

            job.status = 'sending';
            renderUploadQueue();

            const markers = [];
            for (const it of job.files) {
                const id = it.fileId;
                if (!id) continue;

                const f = it.file;
                const encName = encodeURIComponent(it.name || f?.name || `file_${id}`);
                const mime = String(it.mime || f?.type || 'application/octet-stream')
                    .toLowerCase()
                    .replaceAll('|', '/');
                const size = Number(it.size || f?.size || 0);
                markers.push(`[[file:${id}|${encName}|${mime}|${size}]]`);
            }

            const combined = (job.text ? job.text : '') + (markers.length ? ((job.text ? '\n' : '') + markers.join('')) : '');
            await sendMessage(combined, { replyId: job.replyId, replyPreview: job.replyPreview });

            // done
            wsSendState('upload_state', 'stop', job.activity);
            const idx = uploadJobs.findIndex(j => j.jobId === job.jobId);
            if (idx >= 0) uploadJobs.splice(idx, 1);
            renderUploadQueue();
        } catch (e) {
            if (String(e?.message || '').includes('canceled') || String(e?.message || '').includes('aborted')) {
                job.status = 'canceled';
            } else {
                job.status = 'failed';
                job.err = 'Не удалось отправить';
            }
            wsSendState('upload_state', 'stop', job.activity);
            renderUploadQueue();
        }
    };

    form.addEventListener('submit', async (e) => {
        e.preventDefault();
        e.stopImmediatePropagation();

        const text = (input?.value || '').trim();
        const files = pending.slice();

        if (!text && files.length === 0) return;
        if (!currentChatId) {
            alert('Ошибка: не выбран чат для отправки');
            return;
        }

        // snapshot reply
        const replyIdSnap = replyToMessageId;
        const replyPreviewSnap = replyToPreview ? { ...replyToPreview } : null;
        if (replyIdSnap) {
            replyToMessageId = null;
            replyToPreview = null;
            if (replyBarEl) replyBarEl.hidden = true;
        }

        // optimistic UI
        if (input) input.value = '';
        pending = [];
        renderPending();

        const emptyMsg = document.getElementById('messages')?.querySelector?.('.empty-chat');
        if (emptyMsg) emptyMsg.remove();

        // Text-only: keep old behavior (short lock only)
        if (files.length === 0) {
            if (isSubmitting) return;
            isSubmitting = true;
            if (sendBtn) sendBtn.disabled = true;
            try {
                await sendMessage(text, { replyId: replyIdSnap, replyPreview: replyPreviewSnap });
            } catch (error) {
                console.error('[APP] Failed to send message:', error);
                if (input && text) input.value = text;
                showToast('Не удалось отправить');
            } finally {
                isSubmitting = false;
                if (sendBtn) sendBtn.disabled = false;
            }
            return;
        }

        // Files: create background upload job (do NOT lock composer)
        const job = {
            jobId: uploadSeq++,
            chatId: Number(currentChatId),
            serverId: currentServerId ? Number(currentServerId) : null,
            text: text,
            replyId: replyIdSnap || null,
            replyPreview: replyPreviewSnap || null,
            files: files.map(it => ({
                file: it.file,
                name: it.name,
                mime: it.mime,
                size: it.size
            })),
            status: 'uploading',
            loadedBytes: 0,
            totalBytes: 0,
            xhrs: [],
            activity: guessUploadActivity(files)
        };

        uploadJobs.push(job);
        renderUploadQueue();

        // stop typing (user submitted)
        wsSendState('typing', 'stop', 'text');

        startUploadJob(job);
    });

    // ===== Typing (client -> WS) =====
    let typingActive = false;
    let typingStopTimer = null;

    const touchTyping = () => {
        if (!currentChatId) return;
        if (!typingActive) {
            typingActive = true;
            wsSendState('typing', 'start', 'text');
        }
        if (typingStopTimer) clearTimeout(typingStopTimer);
        typingStopTimer = setTimeout(() => {
            typingActive = false;
            wsSendState('typing', 'stop', 'text');
        }, 1500);
    };

    input?.addEventListener('input', touchTyping);
    input?.addEventListener('blur', () => {
        if (!typingActive) return;
        typingActive = false;
        wsSendState('typing', 'stop', 'text');
    });

    window.addEventListener('beforeunload', () => {
        try { wsSendState('typing', 'stop', 'text'); } catch (_) {}
    }, { once: true });

    input?.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault();
            form.dispatchEvent(new Event('submit'));
        }
    });

    console.log('[APP] Message composer setup complete');
}

function normalizeTimestampToMs(ts) {
    if (ts === null || ts === undefined) return Date.now();

    if (typeof ts === 'number') {
        return ts < 1e12 ? ts * 1000 : ts;
    }

    if (typeof ts === 'string') {
        const n = Number(ts);
        if (!Number.isNaN(n)) {
            return n < 1e12 ? n * 1000 : n;
        }
        const parsed = Date.parse(ts);
        return Number.isNaN(parsed) ? Date.now() : parsed;
    }

    return Date.now();
}

function formatMessageTime(ts) {
    const ms = normalizeTimestampToMs(ts);
    return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}


function escapeHtml(s) {
    return String(s ?? '')
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#39;');
}

function showToast(text) {
    const t = (text || '').toString().trim();
    if (!t) return;
    try {
        const el = document.createElement('div');
        el.className = 'lb-toast';
        el.textContent = t;
        document.body.appendChild(el);
        setTimeout(() => {
            try { el.remove(); } catch (_) {}
        }, 1200);
    } catch (_) {}
}

function formatBytes(bytes) {
    const n = Number(bytes);
    if (!Number.isFinite(n) || n <= 0) return '';
    const units = ['B', 'KB', 'MB', 'GB'];
    let v = n;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
        v /= 1024;
        i++;
    }
    const s = (i === 0) ? String(Math.round(v)) : v.toFixed(v >= 10 ? 1 : 2);
    return `${s} ${units[i]}`;
}

function ensureAttachmentViewer() {
    if (attachmentViewer) return attachmentViewer;

    const overlay = document.createElement('div');
    overlay.id = 'attachmentOverlay';
    overlay.className = 'modal-overlay hidden';

    overlay.innerHTML = `
      <div class="attachment-viewer" role="dialog" aria-modal="true">
        <div class="av-topbar">
          <div class="av-left">
            <div class="av-sender" id="avSender"></div>
            <div class="av-time" id="avTime"></div>
            <div class="av-filename" id="avFileName"></div>
            <div class="av-meta" id="avMeta"></div>
          </div>
          <div class="av-actions">
            <button class="av-icon" id="avZoomIn" type="button" data-tip="Приблизить" aria-label="Приблизить">
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M11 4a7 7 0 105.25 11.62L20 19.38" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/>
                <path d="M11 8v6M8 11h6" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/>
              </svg>
            </button>
            <button class="av-icon" id="avZoomOut" type="button" data-tip="Отдалить" aria-label="Отдалить">
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M11 4a7 7 0 105.25 11.62L20 19.38" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/>
                <path d="M8 11h6" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/>
              </svg>
            </button>
            <a class="av-icon" id="avOpen" href="#" target="_blank" rel="noopener" data-tip="Открыть в браузере" aria-label="Открыть в браузере">
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M14 3h7v7" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/>
                <path d="M10 14L21 3" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/>
                <path d="M21 14v6a1 1 0 01-1 1H4a1 1 0 01-1-1V4a1 1 0 011-1h6" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/>
              </svg>
            </a>
            <a class="av-icon" id="avDownload" href="#" download data-tip="Скачать" aria-label="Скачать">
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M12 3v10m0 0l-4-4m4 4l4-4M4 17v3h16v-3"
                      fill="none" stroke="currentColor" stroke-width="3.2" stroke-linecap="round" stroke-linejoin="round"/>
              </svg>
            </a>
            <button class="av-icon" id="avClose" type="button" data-tip="Закрыть" aria-label="Закрыть">
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M18 6L6 18M6 6l12 12" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"/>
              </svg>
            </button>
          </div>
        </div>
        <div class="av-body" id="avBody"></div>
      </div>
    `;


    document.body.appendChild(overlay);

    const close = () => overlay.classList.add('hidden');
    overlay.addEventListener('click', (e) => {
        if (e.target === overlay) close();
    });
    document.addEventListener('keydown', (e) => {
        if (e.key === 'Escape' && !overlay.classList.contains('hidden')) close();
    });

    attachmentViewer = {
        overlay,
        viewerEl: overlay.querySelector('.attachment-viewer'),
        topbarEl: overlay.querySelector('.av-topbar'),
        senderEl: overlay.querySelector('#avSender'),
        timeEl: overlay.querySelector('#avTime'),
        nameEl: overlay.querySelector('#avFileName'),
        metaEl: overlay.querySelector('#avMeta'),
        bodyEl: overlay.querySelector('#avBody'),
        dlEl: overlay.querySelector('#avDownload'),
        openEl: overlay.querySelector('#avOpen'),
        zoomInEl: overlay.querySelector('#avZoomIn'),
        zoomOutEl: overlay.querySelector('#avZoomOut'),
        zoom: 1,
        mediaEl: null,

        setZoom(next) {
            const z = Math.max(1, Math.min(4, Number(next) || 1));
            this.zoom = z;

            const img = this.mediaEl;
            if (img && img.tagName === 'IMG') {
                if (z === 1) {
                    img.style.transform = '';
                    img.classList.remove('zoomed');
                } else {
                    img.style.transform = `scale(${z})`;
                    img.classList.add('zoomed');
                }
            }
            if (this.bodyEl) this.bodyEl.classList.toggle('zoomed', z > 1);
        },

        bumpZoom(delta) {
            this.setZoom((this.zoom || 1) + delta);
        },

        setZoomButtons(enabled) {
            if (this.zoomInEl) this.zoomInEl.disabled = !enabled;
            if (this.zoomOutEl) this.zoomOutEl.disabled = !enabled;
        },

        resetViewerSize() {
            try {
                if (this.viewerEl) {
                    this.viewerEl.style.width = '';
                    this.viewerEl.style.height = '';
                }
            } catch (_) {}
        },

        fitViewerToImage(img) {
            try {
                if (!this.viewerEl || !img) return;
                const nw = img.naturalWidth || 0;
                const nh = img.naturalHeight || 0;
                if (!nw || !nh) return;

                const pad = 24;
                const vw = Math.max(320, window.innerWidth || 0);
                const vh = Math.max(320, window.innerHeight || 0);
                const topH = (this.topbarEl?.offsetHeight || 60) + 2;

                const maxW = Math.max(260, vw - pad);
                const maxH = Math.max(260, vh - pad - topH);

                const scale = Math.min(1, maxW / nw, maxH / nh);
                const w = Math.round(nw * scale);
                const h = Math.round(nh * scale);

                this.viewerEl.style.width = w + 'px';
                this.viewerEl.style.height = (h + topH) + 'px';
            } catch (_) {}
        },

        async open(file) {
            const id = file?.id;
            if (!id) return;

            this.resetViewerSize();

            const name = (file?.name || `file_${id}`).toString();
            const mime = (file?.mime || '').toString().toLowerCase();
            const size = file?.size;

            const sender = (file?.sender || '').toString().trim();
            const time = (file?.time || '').toString().trim();

            if (this.senderEl) this.senderEl.textContent = sender || '';
            if (this.timeEl) this.timeEl.textContent = time ? time : '';
            if (this.nameEl) this.nameEl.textContent = name;

            if (this.metaEl) {
                const badge = (() => {
                    const ext = name.includes('.') ? name.split('.').pop() : '';
                    const up = (ext || '').toString().trim().toUpperCase();
                    if (up && up.length <= 6) return up;
                    const p = mime.toUpperCase().split('/');
                    return (p[1] || p[0] || 'FILE').slice(0, 6);
                })();
                const sz = formatBytes(size);
                this.metaEl.textContent = [badge, sz].filter(Boolean).join(' • ');
            }

let rawHref = `/api/files/${id}/raw`;
let dlHref = `/api/files/${id}`;
let previewHref = `/api/files/${id}/preview`;
try {
    const links = await getFileLinks(id);
    if (links?.raw_url) rawHref = links.raw_url;
    if (links?.download_url) dlHref = links.download_url;
    if (links?.preview_url) previewHref = links.preview_url;
} catch (_) {}

if (this.dlEl) this.dlEl.href = dlHref;
if (this.openEl) this.openEl.href = rawHref;

            if (this.bodyEl) {
                this.bodyEl.innerHTML = '';
                this.mediaEl = null;
                this.setZoom(1);

                if (mime.startsWith('image/')) {
                    const img = document.createElement('img');
                    img.className = 'av-media av-img';
                    img.alt = name;
                    img.src = rawHref;
                    img.loading = 'lazy';
                    img.decoding = 'async';
                    img.addEventListener('load', () => { if (this.zoom === 1) this.fitViewerToImage(img); });
                    img.addEventListener('dblclick', () => {
                        if (this.zoom === 1) this.setZoom(2);
                        else this.setZoom(1);
                    });
                    this.bodyEl.appendChild(img);
                    this.mediaEl = img;
                    this.setZoomButtons(true);
                } else if (mime.startsWith('video/')) {
                    const v = document.createElement('video');
                    v.className = 'av-media av-video';
                    v.controls = true;
                    v.preload = 'metadata';
                    v.src = rawHref;
                    this.bodyEl.appendChild(v);
                    this.mediaEl = v;
                    this.setZoomButtons(false);
                } else if (mime.startsWith('audio/')) {
                    const a = document.createElement('audio');
                    a.className = 'av-media av-audio';
                    a.controls = true;
                    a.preload = 'metadata';
                    a.src = rawHref;
                    this.bodyEl.appendChild(a);
                    this.mediaEl = a;
                    this.setZoomButtons(false);
                } else {
                    const box = document.createElement('div');
                    box.className = 'av-unknown';
                    box.innerHTML = `<div class="av-unknown-name">${escapeHtml(name)}</div><div class="av-unknown-hint">Этот тип файла нельзя открыть в предпросмотре.</div>`;
                    this.bodyEl.appendChild(box);
                    this.setZoomButtons(false);
                }
            }

            overlay.classList.remove('hidden');
        },
    };

    // buttons
    overlay.querySelector('#avClose')?.addEventListener('click', close);
    overlay.querySelector('#avZoomIn')?.addEventListener('click', () => {
        const v = attachmentViewer;
        if (v && v.mediaEl && v.mediaEl.tagName === 'IMG') v.bumpZoom(0.25);
    });
    overlay.querySelector('#avZoomOut')?.addEventListener('click', () => {
        const v = attachmentViewer;
        if (v && v.mediaEl && v.mediaEl.tagName === 'IMG') v.bumpZoom(-0.25);
    });

    window.addEventListener('resize', () => {
        try {
            const v = attachmentViewer;
            if (!v || !v.overlay || v.overlay.classList.contains('hidden')) return;
            if (v.mediaEl && v.mediaEl.tagName === 'IMG' && v.zoom === 1) {
                v.fitViewerToImage(v.mediaEl);
            }
        } catch (_) {}
    });

    return attachmentViewer;
}

// ===== Archive browser (safe, read-only) =====
let archiveViewer = null;

function ensureArchiveViewer() {
    if (archiveViewer) return archiveViewer;

    const overlay = document.createElement('div');
    overlay.id = 'archiveOverlay';
    overlay.className = 'modal-overlay hidden';
    overlay.innerHTML = `
      <div class="archive-viewer" role="dialog" aria-modal="true">
        <div class="av-topbar">
          <div class="av-left">
            <div class="av-filename" id="arName"></div>
            <div class="av-meta" id="arMeta"></div>
          </div>
          <div class="av-actions">
            <button class="av-icon" id="arClose" type="button" data-tip="Закрыть" aria-label="Закрыть">
              <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M18 6L6 18M6 6l12 12" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"/></svg>
            </button>
          </div>
        </div>
        <div class="archive-body" id="arBody"></div>
      </div>
    `;
    document.body.appendChild(overlay);

    const close = () => overlay.classList.add('hidden');
    overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });
    overlay.querySelector('#arClose')?.addEventListener('click', close);
    document.addEventListener('keydown', (e) => {
        if (e.key === 'Escape' && !overlay.classList.contains('hidden')) close();
    });

    archiveViewer = {
        overlay,
        nameEl: overlay.querySelector('#arName'),
        metaEl: overlay.querySelector('#arMeta'),
        bodyEl: overlay.querySelector('#arBody'),
        close,
    };
    return archiveViewer;
}

function buildArchiveTree(entries) {
    // entries: array of {path, size?, is_dir?} OR string paths
    const root = { name: '', kids: new Map(), isDir: true };

    const addPath = (p, size) => {
        const parts = p.split('/').filter(Boolean);
        let cur = root;
        for (let i = 0; i < parts.length; i++) {
            const part = parts[i];
            const last = i === parts.length - 1;
            if (!cur.kids.has(part)) {
                cur.kids.set(part, { name: part, kids: new Map(), isDir: !last, size: last ? size : 0 });
            }
            cur = cur.kids.get(part);
            if (last) {
                cur.isDir = false;
                if (size) cur.size = size;
            }
        }
    };

    for (const e of (entries || [])) {
        if (!e) continue;
        if (typeof e === 'string') addPath(e, 0);
        else if (typeof e === 'object') {
            const p = (e.path || e.name || '').toString();
            if (!p) continue;
            if (e.is_dir) addPath(p.replace(/\/$/, '') + '/', 0);
            else addPath(p, Number(e.size) || 0);
        }
    }

    return root;
}

function renderArchiveNode(node) {
    const ul = document.createElement('ul');
    ul.className = 'archive-tree';
    const kids = Array.from(node.kids.values()).sort((a, b) => {
        if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
        return a.name.localeCompare(b.name);
    });

    for (const k of kids) {
        const li = document.createElement('li');
        if (k.isDir) {
            li.className = 'dir';
            li.innerHTML = `<details open><summary>📁 ${escapeHtml(k.name)}</summary></details>`;
            const det = li.querySelector('details');
            det?.appendChild(renderArchiveNode(k));
        } else {
            li.className = 'file';
            const meta = k.size ? ` <span class="muted">(${escapeHtml(formatBytes(k.size))})</span>` : '';
            li.innerHTML = `📄 ${escapeHtml(k.name)}${meta}`;
        }
        ul.appendChild(li);
    }
    return ul;
}

async function openArchiveViewer({ id, name }) {
    const v = ensureArchiveViewer();
    if (!v) return;
    v.nameEl.textContent = name || 'Архив';
    v.metaEl.textContent = 'Загрузка…';
    v.bodyEl.innerHTML = '';
    v.overlay.classList.remove('hidden');

    try {
        const res = await api(`/api/files/${id}/archive`);
        const entries = Array.isArray(res) ? res : (res?.entries || res?.files || []);
        const tree = buildArchiveTree(entries);
        v.bodyEl.innerHTML = '';
        v.bodyEl.appendChild(renderArchiveNode(tree));
        v.metaEl.textContent = `Файлов: ${entries.length}`;
    } catch (e) {
        v.metaEl.textContent = '';
        v.bodyEl.innerHTML = `<div class="muted" style="padding:12px;">Просмотр содержимого недоступен. Нужен эндпоинт <b>/api/files/:id/archive</b> (минимум для .zip).</div>`;
    }
}

function setupAttachmentUi() {
    if (attachmentUiReady) return;
    const container = $("messages");
    if (!container) return;

    // open viewer (image click / file-row click)
    container.addEventListener('click', (e) => {
        const dl = e.target?.closest?.('.att-dl');
        if (dl) return; // download

        const archBtn = e.target?.closest?.('.att-archive');
        if (archBtn) {
            e.preventDefault();
            e.stopPropagation();
            const att = archBtn.closest('.msg-attachment');
            if (!att) return;
            openArchiveViewer({
                id: att.getAttribute('data-file-id'),
                name: att.getAttribute('data-file-name')
            });
            return;
        }

        const img = e.target?.closest?.('img.att-img');
        if (img) {
            const att = img.closest('.msg-attachment');
            if (!att) return;
            const v = ensureAttachmentViewer();
            const msgEl = att.closest('.message');
            const sender = msgEl?.querySelector('.author .name')?.textContent?.trim() || '';
            const time = msgEl?.querySelector('.author .msg-time')?.textContent?.trim() || '';
            v.open({
                id: att.getAttribute('data-file-id'),
                name: att.getAttribute('data-file-name'),
                mime: att.getAttribute('data-file-mime'),
                size: att.getAttribute('data-file-size'),
                sender,
                time,
            });
            return;
        }

        const row = e.target?.closest?.('.msg-attachment .file-row');
        if (row) {
            const att = row.closest('.msg-attachment');
            if (!att) return;
            const v = ensureAttachmentViewer();
            const msgEl = att.closest('.message');
            const sender = msgEl?.querySelector('.author .name')?.textContent?.trim() || '';
            const time = msgEl?.querySelector('.author .msg-time')?.textContent?.trim() || '';
            v.open({
                id: att.getAttribute('data-file-id'),
                name: att.getAttribute('data-file-name'),
                mime: att.getAttribute('data-file-mime'),
                size: att.getAttribute('data-file-size'),
                sender,
                time,
            });
        }
    });

    // video: dblclick to open viewer
    container.addEventListener('dblclick', (e) => {
        const vtag = e.target?.closest?.('video.att-video');
        if (!vtag) return;
        const att = vtag.closest('.msg-attachment');
        if (!att) return;
        const v = ensureAttachmentViewer();
            const msgEl = att.closest('.message');
            const sender = msgEl?.querySelector('.author .name')?.textContent?.trim() || '';
            const time = msgEl?.querySelector('.author .msg-time')?.textContent?.trim() || '';
            v.open({
            id: att.getAttribute('data-file-id'),
            name: att.getAttribute('data-file-name'),
            mime: att.getAttribute('data-file-mime'),
            size: att.getAttribute('data-file-size'),
                sender,
                time,
        });
    });

    attachmentUiReady = true;
}



// Signed file links (avoid exposing auth tokens in URLs; short-lived per-file dl token)
const _fileLinkCache = new Map(); // id -> { expMs, data }

async function getFileLinks(fileId) {
    const id = String(fileId || '').trim();
    if (!id) return null;

    const now = Date.now();
    const cached = _fileLinkCache.get(id);
    if (cached && cached.expMs && cached.expMs > (now + 3000) && cached.data) {
        return cached.data;
    }

    const data = await api(`/api/files/${encodeURIComponent(id)}/link`);
    const ttl = Number(data?.expires_in_sec || 60);
    const expMs = now + Math.max(1, ttl) * 1000;
    _fileLinkCache.set(id, { expMs, data });
    return data;
}

function wireAttachments(root) {

if (!root) return;

// initialize videos (src + show controls on hover)
root.querySelectorAll?.('video.att-video')?.forEach?.((v) => {
    if (v.dataset.wired === '1') return;
    const src = v.getAttribute('data-src');
    if (src) v.src = src;
    v.controls = false;
    v.addEventListener('mouseenter', () => { v.controls = true; });
    v.addEventListener('mouseleave', () => { v.controls = false; });
    v.dataset.wired = '1';
});

// wire signed links for attachments (download/raw/preview)
root.querySelectorAll?.('.msg-attachment')?.forEach?.((att) => {
    if (!att || att.dataset.linksWired === '1') return;
    const id = att.getAttribute('data-file-id');
    if (!id) return;
    att.dataset.linksWired = '1';

    getFileLinks(id).then((links) => {
        if (!links) return;

        // download buttons (there can be multiple in one attachment)
        att.querySelectorAll?.('a.att-dl')?.forEach?.((a) => {
            if (links.download_url) a.href = links.download_url;
        });

        // image preview + raw
        const img = att.querySelector?.('img.att-img');
        if (img) {
            if (links.preview_url) {
                img.src = links.preview_url;
                try { img.removeAttribute('data-src'); } catch (_) {}
            }
            if (links.raw_url) img.setAttribute('data-raw-src', links.raw_url);
        }

        // video
        const v = att.querySelector?.('video.att-video');
        if (v) {
            if (links.raw_url) v.setAttribute('data-src', links.raw_url);
            if (v.dataset.wired === '1' && links.raw_url) v.src = links.raw_url;
        }

        // audio
        const a = att.querySelector?.('audio.att-audio');
        if (a && links.raw_url) {
            // avoid initial 401: src is set only after we have signed link
            a.src = links.raw_url;
            try { a.removeAttribute('data-src'); } catch (_) {}
        }
    }).catch(() => {
        // ignore
    });
});

}

function renderMessageContent(content) {
    const raw = (content ?? '').toString();

    const renderTextWithLinks = (text) => {
        const s = (text ?? '').toString();
        const urlRe = /\bhttps?:\/\/[^\s<]+/gi;
        if (!urlRe.test(s)) return escapeHtml(s);
        urlRe.lastIndex = 0;
        let out = '';
        let last = 0;
        for (const m of s.matchAll(urlRe)) {
            const start = m.index ?? 0;
            out += escapeHtml(s.slice(last, start));
            const url = m[0];
            const safeUrl = url.replace(/["'<>\s]/g, '');
            out += `<a href="${escapeHtml(safeUrl)}" target="_blank" rel="noopener noreferrer">${escapeHtml(url)}</a>`;
            last = start + url.length;
        }
        out += escapeHtml(s.slice(last));
        return out;
    };

    // Support both canonical and broken legacy markers.
    // canonical: [[file:ID|NAME|MIME|SIZE]]
    // broken:    [[file:ID]]NAME|MIME|SIZE]]
    const reAny = /\[\[file:(\d+)\|([^|]*)\|([^|]*)\|(\d+)\]\]|\[\[file:(\d+)\]\]([^|\]]*)\|([^|\]]*)\|(\d+)\]\]/g;
    if (!reAny.test(raw)) {
        return renderTextWithLinks(raw);
    }

    reAny.lastIndex = 0;

    const dlSvg = `
      <svg class="dl-ico" viewBox="0 0 24 24" aria-hidden="true">
        <path d="M12 3v10m0 0l-4-4m4 4l4-4M4 17v3h16v-3"
              fill="none" stroke="currentColor" stroke-width="3.2" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
    `;

    const fileBadge = (name, mime) => {
        const safe = (name || '').toString();
        const ext = safe.includes('.') ? safe.split('.').pop() : '';
        const up = (ext || '').toString().trim().toUpperCase();
        if (up && up.length <= 6) return up;
        const mm = (mime || '').toString().toUpperCase();
        if (!mm) return 'FILE';
        const p = mm.split('/');
        return (p[1] || p[0] || 'FILE').slice(0, 6);
    };

    let out = '';
    let last = 0;

    for (const m of raw.matchAll(reAny)) {
        const start = m.index ?? 0;
        out += renderTextWithLinks(raw.slice(last, start));

        // canonical groups: 1-4, broken groups: 5-8
        const isCanonical = m[1] !== undefined && m[1] !== null;
        const id = isCanonical ? m[1] : m[5];
        const encName = isCanonical ? (m[2] || '') : (m[6] || '');
        const mime = ((isCanonical ? (m[3] || '') : (m[7] || '')) || '').toLowerCase();
        const size = isCanonical ? m[4] : m[8];

        let name = '';
        try { name = decodeURIComponent(encName); } catch (_) { name = encName; }
        if (!name) name = `file_${id}`;

        const isGif = mime === 'image/gif';
        const isImage = mime.startsWith('image/') && mime !== 'image/svg+xml';
        const isVideo = mime.startsWith('video/');
        const isAudio = mime.startsWith('audio/');
        const isMedia = isImage || isVideo || isAudio;
        const lowerName = (name || '').toString().toLowerCase();
        const isArchive = (!isMedia) && (
            mime === 'application/zip' || mime === 'application/x-zip-compressed' ||
            lowerName.endsWith('.zip') || lowerName.endsWith('.rar') || lowerName.endsWith('.7z') ||
            lowerName.endsWith('.tar') || lowerName.endsWith('.gz') || lowerName.endsWith('.tgz')
        );
        const sizeText = formatBytes(size);

        const href = `/api/files/${id}`; // download
        const rawHref = `/api/files/${id}/raw`; // inline / stream
        const previewHref = isGif ? rawHref : `/api/files/${id}/preview`;
        const badge = fileBadge(name, mime);

        const attData = `data-file-id="${escapeHtml(id)}" data-file-name="${escapeHtml(name)}" data-file-mime="${escapeHtml(mime)}" data-file-size="${escapeHtml(size)}"`;

        out += `<div class="msg-attachment ${isMedia ? 'media' : 'file'} ${isVideo ? 'hover-dl' : ''}" ${attData}>${isImage ? `<div class="att-preview"><img class="att-img" src="data:image/gif;base64,R0lGODlhAQABAAAAACwAAAAAAQABAAA=" data-src="${previewHref}" data-raw-src="${rawHref}" alt="${escapeHtml(name)}" loading="lazy" decoding="async"><a class="att-dl" href="${href}" download title="Скачать">${dlSvg}</a></div>` : ''}${isVideo ? `<div class="att-preview"><video class="att-video" preload="metadata" data-src="${rawHref}"></video><a class="att-dl" href="${href}" download title="Скачать">${dlSvg}</a></div>` : ''}${isAudio ? `<div class="att-preview"><audio class="att-audio" controls preload="metadata" data-src="${rawHref}"></audio><a class="att-dl" href="${href}" download title="Скачать">${dlSvg}</a></div>` : ''}<div class="file-row"><span class="file-badge">${escapeHtml(badge)}</span><span class="file-name" title="${escapeHtml(name)}">${escapeHtml(name)}</span><span class="file-meta">${sizeText ? escapeHtml(sizeText) : ''}</span>${isArchive ? `<button type="button" class="att-archive" data-act="archive" title="Посмотреть содержимое">📦</button>` : ''}<a class="att-dl" href="${href}" download title="Скачать">${dlSvg}</a></div></div>`;

        last = start + m[0].length;
    }

    out += renderTextWithLinks(raw.slice(last));
    return out;
}

function addMessage(msg, opts = {}) {
    const dedup = opts.dedup !== false;
    const prepend = !!opts.prepend;
    const forceScroll = !!opts.forceScroll;

    const chatId = (msg && msg.chat_id !== undefined && msg.chat_id !== null)
        ? msg.chat_id
        : currentChatId;

    if (
        dedup &&
        msg && msg.id !== undefined && msg.id !== null &&
        chatId !== null && chatId !== undefined &&
        hasSeen(chatId, msg.id)
    ) {
        return;
    }

    if (msg && msg.id !== undefined && msg.id !== null && chatId !== null && chatId !== undefined) {
        markSeen(chatId, msg.id);
    }

    const div = document.createElement("div");
    div.className = "message";

    if (msg && msg.id !== undefined && msg.id !== null) {
        div.dataset.msgId = String(msg.id);
    }

    const sender = (msg && msg.sender_username) ? String(msg.sender_username) : '?';
    const senderId = (msg && msg.sender_id !== undefined && msg.sender_id !== null) ? Number(msg.sender_id) : null;
    const isCurrentUser = (Number.isFinite(senderId) && senderId > 0 && Number(currentUser?.id) === senderId);
    const timeText = formatMessageTime(msg?.timestamp);
    const avatarChar = (sender.charAt(0) || '?').toUpperCase();
    // ---- reply fallback (если нет reply_preview, но есть reply_to_id) ----
    window.__lbMsgCache = window.__lbMsgCache || new Map(); // id -> msg

    try {
        const mid = Number(msg?.id);
        if (Number.isFinite(mid) && mid > 0) {
            window.__lbMsgCache.set(mid, {
                id: mid,
                sender_username: (msg?.sender_username || '').toString(),
                content: (msg?.content || '').toString(),
            });
        }
    } catch (_) {}

    let reply = msg?.reply_preview || null;

    if (!reply) {
        const rid = Number(msg?.reply_to_id);
        if (Number.isFinite(rid) && rid > 0) {
            const ref = window.__lbMsgCache.get(rid);
            if (ref) {
                reply = {
                    id: rid,
                    sender_username: (ref.sender_username || '').toString(),
                    content: (ref.content || '').toString(),
                };
            } else {
                reply = {
                    id: rid,
                    sender_username: '',
                    content: 'Сообщение…',
                };
            }
            msg.reply_preview = reply;
        }
    }
    // ---- /reply fallback ----

    const hasReply = !!(reply && Number(reply.id) > 0);
    const replyText = hasReply ? (previewTextFromMessageContent(reply.content) || '') : '';
    const replyHtml = hasReply ? `
        <div class="reply-preview" data-reply-to="${escapeHtml(reply.id)}" title="Перейти к сообщению">
        <span class="rp-icon">↪</span>
        <span class="rp-author">${escapeHtml(reply.sender_username || '')}</span>
        <span class="rp-text">${escapeHtml(replyText.slice(0, 140))}</span>
        </div>
    ` : '';

    div.innerHTML = `
        <div class="avatar ${isCurrentUser ? 'you' : ''}">
            ${avatarInnerHtml((msg && msg.sender_avatar_file_id != null) ? msg.sender_avatar_file_id : (isCurrentUser ? currentUserProfile?.avatar_file_id : null), sender)}
        </div>
        <div class="content">
            <div class="author">
                <span class="name">${escapeHtml(sender)}</span>
                ${isCurrentUser ? '<span class="badge">Вы</span>' : ''}
                <span class="msg-time">${escapeHtml(timeText)}</span>
                <div class="msg-tools">
                  <button type="button" class="tool" data-act="reply" title="Ответить">↩</button>
                  <button type="button" class="tool" data-act="copy" title="Копировать">⧉</button>
                  <button type="button" class="tool" data-act="emoji" title="Эмодзи">😊</button>
                  <button type="button" class="tool" data-act="pin" title="Закрепить">📌</button>
                  ${isCurrentUser ? '<button type=\"button\" class=\"tool\" data-act=\"delete\" title=\"Удалить\">🗑</button>' : ''}
                </div>
            </div>
            ${replyHtml}
            <div class="text">${renderMessageContent(msg?.content ?? '')}</div>
            <div class="msg-reactions" hidden></div>
        </div>
    `;

    if (div.querySelector('.msg-attachment')) div.classList.add('has-attachment');
    if (div.querySelector('.msg-attachment.media')) div.classList.add('has-media');

    const container = $("messages");
    if (!container) return;

    const nearBottom = (() => {
        const threshold = 140;
        return (container.scrollHeight - (container.scrollTop + container.clientHeight)) < threshold;
    })();

    if (prepend && container.firstChild) {
        container.insertBefore(div, container.firstChild);
    } else {
        container.appendChild(div);
    }

    wireAttachments(div);

    // user menu on avatar/name
    if (senderId && !isCurrentUser) {
        const avatarEl = div.querySelector('.avatar');
        const nameEl = div.querySelector('.author .name');

        const onOpenMenu = (e) => {
            e.stopPropagation();
            const anchor = e?.currentTarget || avatarEl || div;
            showUserMenu({
                userId: senderId,
                username: sender,
                anchorEl: anchor,
                allowDm: true,
                allowAddFriend: true,
                allowRemoveFriend: false,
            });
        };

        avatarEl?.addEventListener('click', onOpenMenu);
        nameEl?.addEventListener('click', onOpenMenu);
    }

    // tools
    const tools = div.querySelector('.msg-tools');
    tools?.addEventListener('click', async (e) => {
        const btn = e.target?.closest?.('.tool');
        if (!btn) return;
        e.stopPropagation();
        const act = btn.getAttribute('data-act');
        const mid = Number(msg?.id);

        if (act === 'reply') {
            const raw = (msg?.content || '').toString();
            const clean = previewTextFromMessageContent(raw);
            setReplyTo(mid, sender, clean || '');
            document.getElementById('messageInput')?.focus?.();
            return;
        }

        if (act === 'copy') {
            const raw = (msg?.content || '').toString();

            const cleanText = raw
                // canonical / legacy with pipe
                .replace(/\[\[file[:=]\d+\|[^\]]*\]\]/g, '')
                // broken legacy: [[file:ID]]NAME|MIME|SIZE]]
                .replace(/\[\[file:\d+\]\][^\]]*\]\]/g, '')
                .replace(/\s+/g, ' ')
                .trim();

            let toCopy = cleanText;

            // If message is ONLY attachments — copy file names (without internal ids)
            if (!toCopy && (raw.includes('[[file:') || raw.includes('[[file='))) {
                const names = extractAllFileNamesFromMessageContent(raw);
                if (names && names.length) {
                    toCopy = names.map(n => `📎 ${n}`).join('\n');
                } else {
                    const one = extractFileNameFromMessageContent(raw);
                    toCopy = one ? `📎 ${one}` : '📎 вложение';
                }
            }

            if (!toCopy) toCopy = previewTextFromMessageContent(raw) || '';

            try {
                await navigator.clipboard.writeText(toCopy || '');
            } catch (_) {
                try {
                    const ta = document.createElement('textarea');
                    ta.value = toCopy || '';
                    document.body.appendChild(ta);
                    ta.select();
                    document.execCommand('copy');
                    ta.remove();
                } catch (_) {}
            }
            return;
        }

        if (act === 'emoji') {
            showEmojiPicker({ anchorEl: btn, messageId: mid });
            return;
        }

        if (act === 'pin') {
            if (!Number.isFinite(mid) || mid <= 0) return;
            const wasPinned = btn.classList.contains('active');
            try {
                await api(`/api/messages/${mid}/pin`, { method: wasPinned ? 'DELETE' : 'PUT' });
                btn.classList.toggle('active', !wasPinned);
                showToast(wasPinned ? 'Сообщение откреплено' : 'Сообщение закреплено');
                try {
                    const ov = document.querySelector('.pins-overlay');
                    if (ov && !ov.hidden && Number(currentChatId) > 0) {
                        openPinsModal();
                    }
                } catch (_) {}
            } catch (err) {
                console.warn('[UI] pin toggle failed', err);
                showToast('Не удалось изменить закреп');
            }
            return;
        }

        if (act === 'delete') {
            if (!Number.isFinite(mid) || mid <= 0) return;
            if (!isCurrentUser) return;
            const ok = await askConfirmModal({ title: 'Удаление сообщения', text: 'Удалить сообщение? Это действие нельзя отменить.', okText: 'Удалить', cancelText: 'Отмена', danger: true });
            if (!ok) return;
            try {
                await api(`/api/messages/${mid}`, { method: 'DELETE' });
            } catch (err) {
                console.warn('[UI] delete message failed', err);
                return;
            }
            try {
                div.remove();
            } catch (_) {}
            return;
        }

    });
    // reactions (render + interactions)
    const midForReactions = Number(msg?.id);
    const reactionsEl = div.querySelector('.msg-reactions');
    if (reactionsEl && Number.isFinite(midForReactions) && midForReactions > 0) {
        reactionsEl.addEventListener('click', async (e) => {
            const addBtn = e.target?.closest?.('.react-add');
            if (addBtn) {
                e.stopPropagation();
                showEmojiPicker({ anchorEl: addBtn, messageId: midForReactions });
                return;
            }

            const pill = e.target?.closest?.('.react-pill');
            if (!pill) return;
            e.stopPropagation();

            const emoji = (pill.getAttribute('data-emoji') || '').toString();
            if (!emoji) return;
            const me = (pill.getAttribute('data-me') || '') === '1' || pill.classList.contains('me');

            try {
                if (me) {
                    await api(`/api/messages/${midForReactions}/reactions/${encodeURIComponent(emoji)}`, { method: 'DELETE' });
                } else {
                    await api(`/api/messages/${midForReactions}/reactions/${encodeURIComponent(emoji)}`, { method: 'PUT' });
                }
            } catch (err) {
                console.warn('[UI] toggle reaction failed', err);
            } finally {
                refreshMessageReactions(midForReactions, { force: true });
            }
        });

        if (Array.isArray(msg?.reactions) && msg.reactions.length) {
            applyReactionsToMessageEl(div, msg.reactions);
            _lbReactionsFetched.add(midForReactions);
        }
    }

    // reply preview jump
    div.querySelector('.reply-preview')?.addEventListener('click', async (e) => {
        const rid = Number(e.currentTarget?.getAttribute('data-reply-to'));
        if (!Number.isFinite(rid) || rid <= 0) return;
        const container = document.getElementById('messages');
        if (!container) return;

        const flash = (el) => {
            try {
                el.scrollIntoView({ block: 'center' });
                el.classList.add('flash');
                setTimeout(() => { try { el.classList.remove('flash'); } catch (_) {} }, 800);
            } catch (_) {}
        };

        let anchor = container.querySelector(`.message[data-msg-id="${rid}"]`);
        if (anchor) {
            flash(anchor);
            return;
        }

        // If original message is older than currently loaded history — подгрузим вверх.
        // Cap to avoid infinite loops.
        let tries = 0;
        while (!anchor && chatPaging && chatPaging.hasMore && Number(chatPaging.minId || 0) > rid && tries < 12) {
            tries += 1;
            await loadOlderMessages();
            anchor = container.querySelector(`.message[data-msg-id="${rid}"]`);
        }

        if (anchor) {
            flash(anchor);
            return;
        }

        // not found (deleted or too old)
        try { showToast('Сообщение не найдено'); } catch (_) {}
    });

    const isHistory = !!opts.history || prepend;
    const isCurrentChat = chatId === currentChatId;

    // unread bookkeeping for realtime messages
    if (!isHistory && msg && msg.id !== undefined && msg.id !== null && currentServerId && chatId !== null && chatId !== undefined) {
        const watching = isCurrentChat && isUserWatchingChat(chatId);
        if (watching && (forceScroll || nearBottom)) {
            // user is at bottom while watching -> read instantly
            setLastSeenId(currentServerId, chatId, msg.id);
            clearUnreadCount(currentServerId, chatId);
                        updateChannelListItemOnMessage(chatId, 0);
            removeNewMarker(container);
        } else {
            incUnreadCount(currentServerId, chatId, 1);

            // if user is inside this chat but not at bottom - show marker at last seen
            if (isCurrentChat && !nearBottom) {
                const lastSeen = getLastSeenId(currentServerId, chatId);
                if (lastSeen !== null) insertNewMarkerAfter(container, lastSeen);
            }
        }
        updateJumpBtn();
    }

    if (!prepend && (forceScroll || nearBottom)) {
        setStickToBottom(true);
        scrollToBottomSafe(container, 4);

        // if we scrolled to bottom, also mark read
        const lastId = msg?.id ?? getLatestRenderedMessageId(container);
        if (!isHistory && isCurrentChat && lastId !== null && currentServerId) {
            setLastSeenId(currentServerId, chatId, lastId);
            clearUnreadCount(currentServerId, chatId);
                        updateChannelListItemOnMessage(chatId, 0);
            removeNewMarker(container);
            updateJumpBtn();
        }
    }
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
        normalizeHash();
        applyTheme(localStorage.getItem('theme') || 'dark');
        await loadMe();
        try { window.lbMe = currentUser; } catch (_) {}
        try { initVoice({ wsManager, api, getMe: () => currentUser }); } catch (e) { console.warn('[VOICE] initVoice failed', e); }

        settingsUI = createSettingsUI({
            applyTheme,
            applyMyStatusToUI,
            updateMyStatus,
            getCurrentUser: () => currentUser,
            setCurrentUser: (u) => { currentUser = u; }
        });

        await settingsUI.loadAndApply();

        settingsSnapshot = settingsUI.getSettings ? settingsUI.getSettings() : null;
        window.addEventListener('laberry:settings-changed', (ev) => {
            settingsSnapshot = ev?.detail || null;
        });

        await loadMyStatus();
        await initFriends();
        try { initProfileModal({ api, getMe: () => currentUser }); } catch (e) { console.warn('[PROFILE] init failed', e); }

        // global buttons
        document.getElementById('settingsBtn')?.addEventListener('click', openSettings);
        document.getElementById('pinsBtn')?.addEventListener('click', () => openPinsModal());
        document.getElementById('addChannelBtn')?.addEventListener('click', (e) => { e.preventDefault(); e.stopPropagation(); createChannelFlow(); });
        document.getElementById('dmCallBtn')?.addEventListener('click', (e) => { e.preventDefault(); e.stopPropagation(); startDmCall(); });
        document.addEventListener('click', (e) => {
            const t = e.target;
            if (t && t.id === 'addServerBtn') {
                e.preventDefault();
                createServerFlow();
            }
        });

        // mobile drawer buttons
        document.getElementById('mobileServersBtn')?.addEventListener('click', (e) => {
            e.preventDefault();
            e.stopPropagation();
            toggleServersMenu();
        });

        document.getElementById('mobileChannelsBtn')?.addEventListener('click', (e) => {
            e.preventDefault();
            e.stopPropagation();
            toggleChannelsMenu();
        });

        document.getElementById('mobileMembersBtn')?.addEventListener('click', (e) => {
            e.preventDefault();
            e.stopPropagation();
            toggleMembersMenu();
        });

        document.getElementById('uiOverlay')?.addEventListener('click', () => {
            closeAllDrawers();
        });


        setupWebSocketHandlers();
        setupMessageComposer();
        setupMessagesInfiniteScroll();
        wireMessagesAutoScroll();
        ensureJumpToPresentBtn();
        updateJumpBtn();
        setupAttachmentUi();
        // mobile drawers are closed by clicking overlay
const servers = await api("/api/servers");
        console.log('[APP] Servers loaded:', servers);
        
        const lastServerId = Number(sessionStorage.getItem("lastServerId"));
        const lastChatId = Number(sessionStorage.getItem("lastChatId"));
        
        console.log('[APP] Restoring from sessionStorage:', {
            lastServerId,
            lastChatId,
            serversCount: servers.length
        });
        
        let serverId = lastServerId;
        if (!serverId || !servers.find(s => s.id === serverId)) {
            serverId = servers[0]?.id;
        }
        
        if (serverId) {
            currentServerId = serverId;
        }
        renderServers(servers);
        
        if (serverId) {
            const chats = await api(`/api/servers/${serverId}/chats`);
            console.log('[APP] Chats loaded:', chats);
            
            renderChannels(chats);
        // update members list
        await loadMembers(serverId);
            
            let chatId = lastChatId;
            const isRestoredOk = chatId && chats.find(c => c.id === chatId && c.kind !== 'voice');
            if (!isRestoredOk) {
                chatId = chats.find(c => c.kind !== 'voice')?.id ?? chats[0]?.id;
            }
            
            if (chatId) {
                const chat = chats.find(c => c.id === chatId);
                await openChat(chatId, chat?.name || 'Unknown');
            }
        }
        
        setTimeout(() => {
            initWebSocket();
        }, 500);

        refreshFriendsStatus();
        
    } catch (error) {
        console.error('[APP] Initialization failed:', error);
        
        if (error.status === 401 || error.message.includes('401') || error.message.includes('Unauthorized')) {
            console.error('[APP] Auth error, clearing token and reloading');
            localStorage.removeItem('auth_token');
            localStorage.removeItem('refresh_token');
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

window.addEventListener('resize', () => {
    if (window.innerWidth > 900) {
        hideChannelsMenu();
    }
});

window.addEventListener('error', (event) => {
    console.error('[GLOBAL ERROR]', event.error);
});

window.addEventListener('unhandledrejection', (event) => {
    console.error('[UNHANDLED PROMISE REJECTION]', event.reason);
});

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
window.hideServersMenu = hideServersMenu;
window.hideChannelsMenu = hideChannelsMenu;
window.hideMembersMenu = hideMembersMenu;
window.closeAllDrawers = closeAllDrawers;

if (window.appInitialized) {
    console.warn('[APP] App already initialized elsewhere');
} else {
    window.appInitialized = true;
}

console.log('[APP] Application script loaded successfully');