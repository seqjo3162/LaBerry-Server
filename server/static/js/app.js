let APP_DEBUG = false;
try {
    APP_DEBUG = typeof window !== 'undefined' && (
        window.DEBUG_APP === true || localStorage.getItem('lb_debug_app') === '1'
    );
} catch (_) {}
const appLog = (...args) => {
    if (APP_DEBUG) console.log(...args);
};

appLog('[APP] Module loading started...');

if (typeof fetch === 'undefined') {
    console.error('[APP] fetch is not available!');
    alert('Ваш браузер устарел или блокирует JavaScript');
    throw new Error('fetch not available');
}

import { api } from "./api.js?v=11";
import { initFriends } from "./friends.js?v=10";
import { wsManager } from "./websocket-manager.js?v=12";
import { createSettingsUI } from "./settings.js?v=16";
import { showUserMenu } from "./user-menu.js?v=10";
import { initVoice } from "./voice.js?v=30";
import { initProfileModal } from "./profile-modal.js?v=13";

appLog('[APP] All imports loaded successfully');

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
const chatServerById = new Map(); // chat_id -> server_id
const SERVER_MUTED_KEY = 'lb:muted_servers:v1';
const DM_PROFILE_PANEL_HIDDEN_KEY = 'lb:dm_profile_panel_hidden:v1';
const DM_CALL_FLOAT_CORNER_KEY = 'lb:dm_call_float_corner:v1';
let mutedServerIds = new Set();
let dmProfilePanelHidden = false;
let activeDmCall = null;
let dmCallFloatWired = false;
let lastVoiceSwitchClick = { id: null, at: 0 };
let lastServersSnapshot = [];
let serverSearchQuery = '';

let settingsSnapshot = null;

const E2EE_PREFIX = '[[e2ee:v1|';
const E2EE_SUFFIX = ']]';
const E2EE_TRUSTED_KEYS_PREFIX = 'lb:e2ee:trusted_public_keys:v1';
let e2eeIdentityPromise = null;
const e2eePublicKeyCache = new Map();
let e2eeMissingKeyWarnedAt = 0;

function e2eeAvailable() {
    return !!(window.crypto?.subtle && window.crypto?.getRandomValues && window.TextEncoder && window.TextDecoder);
}

function e2eeStorageKey() {
    const uid = Number(currentUser?.id);
    return `lb:e2ee:identity:v1:${Number.isFinite(uid) && uid > 0 ? uid : 'anon'}`;
}

function e2eeDeviceIdStorageKey() {
    const uid = Number(currentUser?.id);
    return `lb:e2ee:device_id:v1:${Number.isFinite(uid) && uid > 0 ? uid : 'anon'}`;
}

function e2eeGetOrCreateDeviceId() {
    try {
        let id = localStorage.getItem(e2eeDeviceIdStorageKey());
        if (id && String(id).trim()) return id;
        if (crypto && typeof crypto.randomUUID === 'function') {
            id = crypto.randomUUID();
        } else {
            const a = new Uint8Array(16);
            crypto.getRandomValues(a);
            let s = '';
            for (let i = 0; i < a.length; i += 1) s += (a[i] | 0).toString(16).padStart(2, '0');
            id = s;
        }
        localStorage.setItem(e2eeDeviceIdStorageKey(), id);
        return id;
    } catch (_) {
        return null;
    }
}

function e2eeTrustedKeysStorageKey() {
    const uid = Number(currentUser?.id);
    return `${E2EE_TRUSTED_KEYS_PREFIX}:${Number.isFinite(uid) && uid > 0 ? uid : 'anon'}`;
}

function e2eeIsEncryptedText(text) {
    const raw = (text || '').toString().trim();
    return raw.startsWith(E2EE_PREFIX) && raw.endsWith(E2EE_SUFFIX);
}

function e2eeBytesToB64u(bytes) {
    let bin = '';
    const arr = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes || []);
    for (let i = 0; i < arr.length; i += 1) bin += String.fromCharCode(arr[i]);
    return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
}

function e2eeB64uToBytes(value) {
    const s = String(value || '').replace(/-/g, '+').replace(/_/g, '/');
    const padded = s + '='.repeat((4 - (s.length % 4)) % 4);
    const bin = atob(padded);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
    return out;
}

function e2eeParsePublicKey(value) {
    if (!value) return null;
    if (typeof value === 'object') {
        if (value.ecdh && typeof value.ecdh === 'object') return value.ecdh;
        if (value.publicJwk && typeof value.publicJwk === 'object') return value.publicJwk;
        return value;
    }
    try {
        const parsed = JSON.parse(String(value));
        if (!parsed || typeof parsed !== 'object') return null;
        if (parsed.ecdh && typeof parsed.ecdh === 'object') return parsed.ecdh;
        if (parsed.publicJwk && typeof parsed.publicJwk === 'object') return parsed.publicJwk;
        return parsed;
    } catch (_) {
        return null;
    }
}

function e2eeStableJson(value) {
    if (Array.isArray(value)) {
        return `[${value.map(e2eeStableJson).join(',')}]`;
    }
    if (value && typeof value === 'object') {
        return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${e2eeStableJson(value[key])}`).join(',')}}`;
    }
    return JSON.stringify(value);
}

async function e2eeFingerprintPublicKey(jwk) {
    if (!e2eeAvailable() || !jwk) return null;
    const bytes = new TextEncoder().encode(e2eeStableJson(jwk));
    const digest = await crypto.subtle.digest('SHA-256', bytes);
    return e2eeBytesToB64u(new Uint8Array(digest));
}

function e2eeLoadTrustedKeys() {
    try {
        const raw = JSON.parse(localStorage.getItem(e2eeTrustedKeysStorageKey()) || '{}');
        return raw && typeof raw === 'object' ? raw : {};
    } catch (_) {
        return {};
    }
}

function e2eeSaveTrustedKeys(map) {
    try {
        localStorage.setItem(e2eeTrustedKeysStorageKey(), JSON.stringify(map || {}));
    } catch (_) {}
}

async function e2eeTrustPublicKey(userId, username, jwk) {
    const id = Number(userId);
    if (!Number.isFinite(id) || id <= 0 || id === Number(currentUser?.id)) return;
    const fingerprint = await e2eeFingerprintPublicKey(jwk);
    if (!fingerprint) throw new Error('e2ee_key_fingerprint_failed');

    const trusted = e2eeLoadTrustedKeys();
    const prev = trusted[String(id)];
    if (prev?.fingerprint && prev.fingerprint !== fingerprint) {
        const err = new Error('e2ee_public_key_changed');
        err.code = 'e2ee_public_key_changed';
        err.username = username || `#${id}`;
        throw err;
    }

    if (!prev) {
        trusted[String(id)] = {
            fingerprint,
            username: username || '',
            trusted_at: new Date().toISOString(),
        };
        e2eeSaveTrustedKeys(trusted);
    }
}

function e2eeParseEnvelope(text) {
    const raw = (text || '').toString().trim();
    if (!e2eeIsEncryptedText(raw)) return null;
    try {
        const packed = raw.slice(E2EE_PREFIX.length, -E2EE_SUFFIX.length);
        const json = new TextDecoder().decode(e2eeB64uToBytes(packed));
        const env = JSON.parse(json);
        return env && env.alg === 'LB-E2EE-v1' ? env : null;
    } catch (_) {
        return null;
    }
}

async function e2eeImportPublicKey(jwk) {
    if (!jwk) return null;
    return crypto.subtle.importKey(
        'jwk',
        jwk,
        { name: 'ECDH', namedCurve: 'P-256' },
        false,
        []
    );
}

async function e2eeRegisterDeviceKey(deviceId, publicJwk, label) {
    if (!deviceId || !publicJwk) return;
    try {
        await api('/api/users/me/device-keys', {
            method: 'POST',
            body: JSON.stringify({ device_id: deviceId, public_jwk: JSON.stringify(publicJwk), label: label || null }),
        });
    } catch (e) {
        console.warn('[E2EE] failed to register device key', e);
    }
}

async function e2eeGetUserDeviceKeys(userId) {
    const id = Number(userId);
    if (!Number.isFinite(id) || id <= 0) return [];
    try {
        const rows = await api(`/api/users/${encodeURIComponent(id)}/device-keys`);
        if (!Array.isArray(rows)) return [];
        return rows.map((r) => ({ device_id: r.device_id, public_jwk: e2eeParsePublicKey(r.public_jwk) }));
    } catch (e) {
        return [];
    }
}

async function e2eeEnsureIdentity(upload = false) {
    if (!e2eeAvailable()) return null;
    if (e2eeIdentityPromise) return e2eeIdentityPromise;

    e2eeIdentityPromise = (async () => {
        let saved = null;
        try { saved = JSON.parse(localStorage.getItem(e2eeStorageKey()) || 'null'); } catch (_) { saved = null; }

        let privateJwk = saved?.privateJwk || null;
        let publicJwk = saved?.publicJwk || null;

        if (!privateJwk || !publicJwk) {
            const pair = await crypto.subtle.generateKey(
                { name: 'ECDH', namedCurve: 'P-256' },
                true,
                ['deriveKey']
            );
            privateJwk = await crypto.subtle.exportKey('jwk', pair.privateKey);
            publicJwk = await crypto.subtle.exportKey('jwk', pair.publicKey);
            try { localStorage.setItem(e2eeStorageKey(), JSON.stringify({ privateJwk, publicJwk })); } catch (_) {}
        }

        const privateKey = await crypto.subtle.importKey(
            'jwk',
            privateJwk,
            { name: 'ECDH', namedCurve: 'P-256' },
            false,
            ['deriveKey']
        );

        const publicText = JSON.stringify(publicJwk);
        if (upload && currentUser && currentUser.public_encryption_key !== publicText) {
            try {
                    // register per-device key instead of overwriting account-level key
                    const deviceId = e2eeGetOrCreateDeviceId();
                    await e2eeRegisterDeviceKey(deviceId, publicJwk, navigator.userAgent);
                    // still update account object locally
                    currentUser = { ...currentUser, ...(await api('/api/users/me') || {}), public_encryption_key: publicText };
            } catch (e) {
                console.warn('[E2EE] failed to publish public key', e);
            }
        }

        if (currentUser?.id) {
            e2eePublicKeyCache.set(Number(currentUser.id), publicJwk);
        }

        return { privateKey, publicJwk, publicText };
    })();

    return e2eeIdentityPromise;
}

async function e2eeGetUserPublicKey(userId) {
    const id = Number(userId);
    if (!Number.isFinite(id) || id <= 0) return null;
    if (e2eePublicKeyCache.has(id)) return e2eePublicKeyCache.get(id);

    if (id === Number(currentUser?.id)) {
        const own = await e2eeEnsureIdentity(false);
        if (own?.publicJwk) return own.publicJwk;
    }

    try {
        const u = await api(`/api/users/${encodeURIComponent(id)}`);
        const jwk = e2eeParsePublicKey(u?.public_encryption_key);
        if (jwk) {
            await e2eeTrustPublicKey(id, u?.username, jwk);
            e2eePublicKeyCache.set(id, jwk);
            return jwk;
        }
        // fallback: try to get per-device keys
        const devs = await e2eeGetUserDeviceKeys(id);
        if (devs && devs.length) {
            const first = devs[0].public_jwk;
            if (first) {
                await e2eeTrustPublicKey(id, u?.username, first);
                e2eePublicKeyCache.set(id, first);
                return first;
            }
        }
        return null;
    } catch (e) {
        if (e?.code === 'e2ee_public_key_changed') throw e;
        return null;
    }
}

async function e2eeDeriveWrapKey(publicJwk) {
    const identity = await e2eeEnsureIdentity(false);
    if (!identity || !publicJwk) return null;
    const publicKey = await e2eeImportPublicKey(publicJwk);
    if (!publicKey) return null;
    return crypto.subtle.deriveKey(
        { name: 'ECDH', public: publicKey },
        identity.privateKey,
        { name: 'AES-GCM', length: 256 },
        false,
        ['encrypt', 'decrypt']
    );
}

async function e2eeCurrentChatRecipients() {
    const out = new Map();
    if (currentUser?.id) {
        const own = await e2eeEnsureIdentity(false);
        out.set(Number(currentUser.id), {
            id: Number(currentUser.id),
            username: currentUser.username || 'Вы',
            public_encryption_key: own?.publicText || currentUser.public_encryption_key || null,
        });
    }

    try {
        const rows = currentServerId
            ? await api(`/api/servers/${encodeURIComponent(currentServerId)}/members`)
            : await api(`/api/dms/${encodeURIComponent(currentChatId)}/participants`);
        for (const row of Array.isArray(rows) ? rows : []) {
            const id = Number(row?.id);
            if (!Number.isFinite(id) || id <= 0) continue;
            out.set(id, row);
        }
    } catch (e) {
        console.warn('[E2EE] failed to load recipients', e);
        throw new Error('e2ee_recipients_failed');
    }

    return [...out.values()];
}

function e2eeShouldEncryptOutgoing(content) {
    const raw = (content || '').toString();
    if (!raw.trim()) return false;
    if (e2eeIsEncryptedText(raw)) return false;
    if (raw.includes('[[file:') || raw.includes('[[file=')) return false;
    if (!currentChatId) return false;
    return e2eeAvailable();
}

async function e2eeEncryptForCurrentChat(plaintext) {
    if (!e2eeShouldEncryptOutgoing(plaintext)) return plaintext;
    const identity = await e2eeEnsureIdentity(true);
    if (!identity) throw new Error('e2ee_identity_unavailable');

    let recipients;
    try {
        recipients = await e2eeCurrentChatRecipients();
    } catch (e) {
        console.warn('[E2EE] Failed to load recipients:', e);
        recipients = [];
    }
    if (!recipients.length) {
        console.warn('[E2EE] No recipients available, sending plaintext');
        return plaintext;
    }
    const keys = {};
    const missing = [];

    const messageKey = await crypto.subtle.generateKey(
        { name: 'AES-GCM', length: 256 },
        true,
        ['encrypt', 'decrypt']
    );
    const messageKeyRaw = new Uint8Array(await crypto.subtle.exportKey('raw', messageKey));

    // ephemeral sender key for this message
    const ephemeral = await crypto.subtle.generateKey(
        { name: 'ECDH', namedCurve: 'P-256' },
        true,
        ['deriveKey']
    );
    const ephemeralPubJwk = await crypto.subtle.exportKey('jwk', ephemeral.publicKey);

    for (const recipient of recipients) {
        const id = Number(recipient?.id);
        if (!Number.isFinite(id) || id <= 0) continue;

        // gather recipient devices
        let devices = [];
        try {
            devices = await e2eeGetUserDeviceKeys(id);
        } catch (_) { devices = []; }

        // fallback to account-level single key
        if ((!devices || devices.length === 0) && recipient?.public_encryption_key) {
            const parsed = e2eeParsePublicKey(recipient.public_encryption_key);
            if (parsed) devices = [{ device_id: 'server', public_jwk: parsed }];
        }

        if (!devices || devices.length === 0) {
            missing.push(recipient?.username || `#${id}`);
            continue;
        }

        keys[String(id)] = {};

        for (const d of devices) {
            const devId = String(d.device_id || '');
            const jwk = d.public_jwk;
            if (!jwk) {
                continue;
            }
            try {
                const publicKey = await e2eeImportPublicKey(jwk);
                if (!publicKey) continue;
                const wrapKey = await crypto.subtle.deriveKey(
                    { name: 'ECDH', public: publicKey },
                    ephemeral.privateKey,
                    { name: 'AES-GCM', length: 256 },
                    false,
                    ['encrypt', 'decrypt']
                );
                const keyIv = crypto.getRandomValues(new Uint8Array(12));
                const keyCt = new Uint8Array(await crypto.subtle.encrypt({ name: 'AES-GCM', iv: keyIv }, wrapKey, messageKeyRaw));
                keys[String(id)][devId || 'unknown'] = { iv: e2eeBytesToB64u(keyIv), ct: e2eeBytesToB64u(keyCt) };
            } catch (e) {
                console.warn('[E2EE] key wrap failed for device', d, e);
            }
        }
    }

    if (missing.length) {
        const now = Date.now();
        if (now - e2eeMissingKeyWarnedAt > 8000) {
            e2eeMissingKeyWarnedAt = now;
            showToast(`E2EE не применено для: ${missing.slice(0, 3).join(', ') || 'участников'}`);
        }
    }

    // Graceful fallback: если не удалось зашифровать для себя — не блокируем отправку
    if (!keys[String(currentUser?.id)]) {
        console.warn('[E2EE] Cannot encrypt for self, message will be sent without E2EE');
        // НЕ выбрасываем ошибку — позволяем отправить сообщение в открытом виде
    }

    const iv = crypto.getRandomValues(new Uint8Array(12));
    const ct = new Uint8Array(await crypto.subtle.encrypt(
        { name: 'AES-GCM', iv },
        messageKey,
        new TextEncoder().encode(String(plaintext))
    ));

    const envelope = {
        alg: 'LB-E2EE-v1',
        sender: Number(currentUser?.id) || 0,
        sender_key: identity.publicJwk,
        ephemeral: ephemeralPubJwk,
        iv: e2eeBytesToB64u(iv),
        ct: e2eeBytesToB64u(ct),
        keys,
    };

    const packed = e2eeBytesToB64u(new TextEncoder().encode(JSON.stringify(envelope)));
    return `${E2EE_PREFIX}${packed}${E2EE_SUFFIX}`;
}

async function e2eeDecryptText(content) {
    const raw = (content || '').toString();
    const env = e2eeParseEnvelope(raw);
    if (!env) return raw;
    if (!e2eeAvailable()) return '🔒 Сообщение зашифровано. Нужен HTTPS и современный браузер.';

    try {
        const identity = await e2eeEnsureIdentity(false);
        if (!identity || !currentUser?.id) return '🔒 Не удалось открыть ключ шифрования.';

        const myDeviceId = e2eeGetOrCreateDeviceId();
        const userKeyMap = env.keys && env.keys[String(currentUser.id)];

        // backward-compatible: old format had keys[userId] = { iv, ct }
        if (userKeyMap && userKeyMap.iv && userKeyMap.ct) {
            try {
                const senderKey = e2eeParsePublicKey(env.sender_key) || await e2eeGetUserPublicKey(env.sender);
                const wrapKey = await e2eeDeriveWrapKey(senderKey);
                if (!wrapKey) return '🔒 Не удалось получить ключ отправителя.';
                const messageKeyRaw = await crypto.subtle.decrypt(
                    { name: 'AES-GCM', iv: e2eeB64uToBytes(userKeyMap.iv) },
                    wrapKey,
                    e2eeB64uToBytes(userKeyMap.ct)
                );
                const messageKey = await crypto.subtle.importKey('raw', messageKeyRaw, { name: 'AES-GCM' }, false, ['decrypt']);
                const plain = await crypto.subtle.decrypt(
                    { name: 'AES-GCM', iv: e2eeB64uToBytes(env.iv) },
                    messageKey,
                    e2eeB64uToBytes(env.ct)
                );
                return new TextDecoder().decode(plain);
            } catch (e) {
                console.warn('[E2EE] legacy decrypt failed', e);
                return '🔒 Не удалось расшифровать сообщение на этом устройстве.';
            }
        }

        if (!userKeyMap || typeof userKeyMap !== 'object') return '🔒 Сообщение зашифровано не для этого аккаунта или устройства.';

        const wrapped = userKeyMap[String(myDeviceId)] || userKeyMap[myDeviceId] || userKeyMap['unknown'];
        if (!wrapped) return '🔒 Сообщение зашифровано не для этого аккаунта или устройства.';

        try {
            // prefer ephemeral sender key if present
            let wrapKey = null;
            if (env.ephemeral) {
                const senderEphemeral = e2eeParsePublicKey(env.ephemeral) || env.ephemeral;
                const senderEphemeralKey = await e2eeImportPublicKey(senderEphemeral);
                if (senderEphemeralKey) {
                    wrapKey = await crypto.subtle.deriveKey(
                        { name: 'ECDH', public: senderEphemeralKey },
                        identity.privateKey,
                        { name: 'AES-GCM', length: 256 },
                        false,
                        ['decrypt']
                    );
                }
            }

            // fallback to sender static key (legacy)
            if (!wrapKey) {
                const senderKey = e2eeParsePublicKey(env.sender_key) || await e2eeGetUserPublicKey(env.sender);
                wrapKey = await e2eeDeriveWrapKey(senderKey);
            }

            if (!wrapKey) return '🔒 Не удалось получить ключ отправителя.';

            const messageKeyRaw = await crypto.subtle.decrypt(
                { name: 'AES-GCM', iv: e2eeB64uToBytes(wrapped.iv) },
                wrapKey,
                e2eeB64uToBytes(wrapped.ct)
            );
            const messageKey = await crypto.subtle.importKey('raw', messageKeyRaw, { name: 'AES-GCM' }, false, ['decrypt']);
            const plain = await crypto.subtle.decrypt(
                { name: 'AES-GCM', iv: e2eeB64uToBytes(env.iv) },
                messageKey,
                e2eeB64uToBytes(env.ct)
            );
            return new TextDecoder().decode(plain);
        } catch (e) {
            console.warn('[E2EE] decrypt failed', e);
            return '🔒 Не удалось расшифровать сообщение на этом устройстве.';
        }
    } catch (e) {
        console.warn('[E2EE] decrypt failed', e);
        return '🔒 Не удалось расшифровать сообщение на этом устройстве.';
    }
}

async function prepareMessageForDisplay(msg) {
    if (!msg || typeof msg !== 'object') return msg;
    const out = { ...msg };
    out.content = await e2eeDecryptText(out.content);
    if (out.reply_preview && typeof out.reply_preview === 'object') {
        out.reply_preview = {
            ...out.reply_preview,
            content: await e2eeDecryptText(out.reply_preview.content),
        };
    }
    return out;
}

function loadMutedServers() {
    try {
        const raw = JSON.parse(localStorage.getItem(SERVER_MUTED_KEY) || '[]');
        mutedServerIds = new Set((Array.isArray(raw) ? raw : []).map((v) => Number(v)).filter((v) => Number.isFinite(v) && v > 0));
    } catch (_) {
        mutedServerIds = new Set();
    }
}

function saveMutedServers() {
    try { localStorage.setItem(SERVER_MUTED_KEY, JSON.stringify([...mutedServerIds])); } catch (_) {}
}

function isServerMuted(serverId) {
    const sid = Number(serverId);
    return Number.isFinite(sid) && sid > 0 && mutedServerIds.has(sid);
}

function toggleServerMuted(serverId) {
    const sid = Number(serverId);
    if (!Number.isFinite(sid) || sid <= 0) return false;
    if (mutedServerIds.has(sid)) mutedServerIds.delete(sid);
    else mutedServerIds.add(sid);
    saveMutedServers();
    return mutedServerIds.has(sid);
}

function updateServerSelection(serverId) {
    const target = Number(serverId);
    document.querySelectorAll('.item.server').forEach((item) => {
        const itemId = Number(item.dataset.serverId);
        item.classList.toggle('active', Number.isFinite(target) && target > 0 && itemId === target);
    });
}

loadMutedServers();
try {
    dmProfilePanelHidden = localStorage.getItem(DM_PROFILE_PANEL_HIDDEN_KEY) === '1';
} catch (_) {
    dmProfilePanelHidden = false;
}

function avatarRawUrl(fileId) {
    const id = Number(fileId);
    if (!Number.isFinite(id) || id <= 0) return null;
    return `/api/profile-files/${id}/raw`;
}

function avatarInnerHtml(fileId, usernameFallback) {
    const letter = String(usernameFallback || '?').trim().charAt(0).toUpperCase() || '?';
    const url = avatarRawUrl(fileId);
    if (url) {
        const alt = escapeHtml(usernameFallback || '');
        return `<span class="avatar-fallback" aria-hidden="true" style="display:none">${escapeHtml(letter)}</span><img class="avatar-img" src="${url}" alt="${alt}" data-avatar-fallback="${escapeHtml(letter)}">`;
    }
    return escapeHtml(letter);
}

function wireAvatarFallbacks(root = document) {
    // Use a module-level variable to ensure the MutationObserver is set up only once.
    if (window._avatarObserver && window._avatarObserver.isConnected) return;

    const setupImageListeners = (img, fallback) => {
        if (img.dataset.avatarWired === '1') return; // Already wired

        // Set initial state and mark as processed
        img.dataset.avatarWired = '1';

        const showFallback = () => {
            img.style.display = 'none';
            if (fallback) fallback.style.display = '';
            const box = img.closest?.('.avatar, .pin-avatar');
            if (box) box.classList.add('avatar-load-failed');
        };
        const showImage = () => {
            img.style.display = '';
            if (fallback) fallback.style.display = 'none';
            const box = img.closest?.('.avatar, .pin-avatar');
            if (box) box.classList.remove('avatar-load-failed');
        };

        // Attach listeners only once per image element
        img.addEventListener('error', showFallback);
        img.addEventListener('load', showImage);
        
        // Initial check for cached images using naturalWidth > 0
        if (img.complete && img.naturalWidth > 0) {
            showImage();
        } else if (!img.complete || img.naturalWidth === 0) {
             // If not complete or zero width, rely on event listeners for state changes.
        }
    };

    const processNode = (node) => {
        if (!node || node.tagName !== 'IMG') return;
        
        const img = node;
        // Only target images with the specific class and not already wired
        if (img.classList.contains('avatar-img') && !img.dataset.avatarWired) {
            const fallback = img.previousElementSibling?.classList?.contains('avatar-fallback') ? img.previousElementSibling : null;
            setupImageListeners(img, fallback);
        }
    };

    // 1. Initial scan of existing images on the root element
    document.querySelectorAll('.avatar-img:not([data-avatar-wired])').forEach((img) => {
        const fallback = img.previousElementSibling?.classList?.contains('avatar-fallback') ? img.previousElementSibling : null;
        setupImageListeners(img, fallback);
    });

    // 2. Use MutationObserver for dynamic content (only if not already set up)
    if (!window._avatarObserver) {
        const observer = new MutationObserver((mutationsList) => {
            for (const mutation of mutationsList) {
                if (mutation.type === 'childList') {
                    // Process added nodes first
                    mutation.addedNodes?.forEach((node) => {
                        // Check if the added node itself is an image
                        if (node.tagName === 'IMG' && node.classList.contains('avatar-img') && !node.dataset.avatarWired) {
                            const fallback = node.previousElementSibling?.classList?.contains('avatar-fallback') ? node.previousElementSibling : null;
                            setupImageListeners(node, fallback);
                        } else if (node.querySelectorAll) {
                            // Process descendants of the added node
                            node.querySelectorAll('img.avatar-img:not([data-avatar-wired])').forEach((img) => {
                                const fallback = img.previousElementSibling?.classList?.contains('avatar-fallback') ? img.previousElementSibling : null;
                                setupImageListeners(img, fallback);
                            });
                        }
                    });
                }
            }
        });

        // Observe the entire document body for changes in children/subtrees
        observer.observe(document.body, { childList: true, subtree: true });
        window._avatarObserver = observer; // Store the instance globally to prevent re-initialization
    }
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
        img.onload = () => {
            img.style.display = '';
            if (txt) txt.style.display = 'none';
        };
        img.onerror = () => {
            img.removeAttribute('src');
            img.style.display = 'none';
            if (txt) txt.style.display = '';
        };

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

try {
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', () => wireAvatarFallbacks(document), { once: true });
    } else {
        wireAvatarFallbacks(document);
    }

    const avatarObserver = new MutationObserver((items) => {
        for (const item of items) {
            item.addedNodes?.forEach?.((node) => {
                if (node && node.nodeType === 1) wireAvatarFallbacks(node);
            });
        }
    });
    avatarObserver.observe(document.documentElement, { childList: true, subtree: true });
} catch (_) {}

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
    appLog('[APP] Page unloading, disconnecting WS');
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

function formatProfileDate(value) {
    if (!value) return 'Дата регистрации неизвестна';
    const dt = new Date(value);
    if (Number.isNaN(dt.getTime())) return 'Дата регистрации неизвестна';
    try {
        return new Intl.DateTimeFormat('ru-RU', {
            day: '2-digit',
            month: 'long',
            year: 'numeric',
        }).format(dt);
    } catch (_) {
        return dt.toLocaleDateString();
    }
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

let notificationWorkerRegisterPromise = null;

function registerNotificationWorker() {
    if (!('serviceWorker' in navigator)) return null;
    if (location.protocol !== 'https:' && location.hostname !== 'localhost' && location.hostname !== '127.0.0.1') return null;
    if (!notificationWorkerRegisterPromise) {
        notificationWorkerRegisterPromise = navigator.serviceWorker
            .register('/static/sw.js')
            .catch((err) => {
                console.warn('[NOTIFY] service worker registration failed', err);
                notificationWorkerRegisterPromise = null;
                return null;
            });
    }
    return notificationWorkerRegisterPromise;
}

function showInAppNotification({ title, body = '', tag = '', kind = 'message', onClick = null, timeout = 6500 } = {}) {
    if (!title || !document?.body) return;

    let stack = document.getElementById('lbNotifyStack');
    if (!stack) {
        stack = document.createElement('div');
        stack.id = 'lbNotifyStack';
        stack.className = 'lb-notify-stack';
        stack.setAttribute('aria-live', 'polite');
        document.body.appendChild(stack);
    }

    if (tag) {
        stack.querySelectorAll('.lb-notify-card').forEach((item) => {
            if (item.dataset.notifyTag === String(tag)) item.remove();
        });
    }

    const card = document.createElement('button');
    card.type = 'button';
    card.className = `lb-notify-card ${kind ? `is-${kind}` : ''}`;
    if (tag) card.dataset.notifyTag = String(tag);
    card.innerHTML = `
        <span class="lb-notify-accent" aria-hidden="true"></span>
        <span class="lb-notify-main">
            <span class="lb-notify-title">${escapeHtml(title)}</span>
            ${body ? `<span class="lb-notify-body">${escapeHtml(body)}</span>` : ''}
        </span>
    `;

    const close = () => {
        card.classList.add('is-leaving');
        setTimeout(() => card.remove(), 180);
    };

    card.addEventListener('click', () => {
        try { if (typeof onClick === 'function') onClick(); } catch (_) {}
        close();
    });

    stack.appendChild(card);
    const ttl = Number(timeout);
    if (Number.isFinite(ttl) && ttl > 0) setTimeout(close, ttl);
}

function showDesktopNotification(title, body, tag, data = {}) {
    const { desktop } = canNotifyNow();
    if (!desktop) return;

    const now = Date.now();
    if (now - lastDesktopAt < 700) return;

    const options = {
        body,
        tag: tag ? String(tag) : undefined,
        silent: true, // sound handled separately
        data: {
            url: '/app',
            ...(data || {}),
        },
        requireInteraction: !!data?.requireInteraction,
    };

    try {
        if ('serviceWorker' in navigator) {
            registerNotificationWorker();
            navigator.serviceWorker.ready
                .then((reg) => reg.showNotification(title, options))
                .catch(() => {
                    const n = new Notification(title, options);
                    n.onclick = () => {
                        try { window.focus(); } catch (_) {}
                    };
                });
            lastDesktopAt = now;
            return;
        }

        const n = new Notification(title, {
            ...options,
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

    const roomServerId = Number(chatServerById.get(Number(roomId)) || 0);
    if (roomServerId > 0 && isServerMuted(roomServerId)) return;

    // client-side unread counter (for Discord-like open behavior)
    if (currentServerId && roomId !== null && roomId !== undefined) {
        incUnreadCount(currentServerId, roomId, 1);
        updateJumpBtn();
    }

    // desktop notification + sound use same trigger
    showInAppNotification({
        title,
        body,
        tag: `chat:${roomId}`,
        kind: 'message',
        onClick: () => {
            try { window.focus(); } catch (_) {}
            try { openChat(roomId, chatNameById.get(roomId) || `Chat #${roomId}`).catch(() => {}); } catch (_) {}
        },
    });
    showDesktopNotification(title, body, `chat:${roomId}`, { chatId: roomId, url: '/app' });
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

async function openSettings(section) {
    try { hideChannelsMenu(); } catch (_) {}
    if (settingsUI) {
        const target = typeof section === 'string' ? section : undefined;
        settingsUI.open(target);
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
        state: document.getElementById('dmCallState'),
        accept: document.getElementById('dmCallAcceptBtn'),
        decline: document.getElementById('dmCallDeclineBtn'),
    };
}

function setDmCallOverlay(info, mode = 'incoming') {
    const { overlay, avatar, name, title, sub, state, accept, decline } = dmCallOverlayEls();
    if (!overlay) return;

    dmCallIncoming = info || null;
    dmCallOverlayMode = mode || 'incoming';

    const displayName = (info?.from_username || info?.target_username || chatNameById.get(Number(info?.chat_id)) || 'Пользователь').toString();
    const letter = (displayName.trim().charAt(0) || 'U').toUpperCase();

    if (avatar) avatar.textContent = letter;
    if (name) name.textContent = displayName;

    if (mode === 'outgoing') {
        if (state) state.textContent = 'Исходящий';
        if (title) title.textContent = 'Исходящий звонок';
        if (sub) sub.textContent = 'Ожидаем ответ и держим линию открытой.';
        if (accept) accept.hidden = true;
        if (decline) decline.textContent = 'Отменить';
    } else {
        if (state) state.textContent = 'Входящий';
        if (title) title.textContent = 'Входящий звонок';
        if (sub) sub.textContent = 'Ответьте, чтобы сразу перейти в голосовой диалог.';
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

function dmCallFloatEls() {
    return {
        root: document.getElementById('dmCallFloat'),
        grip: document.getElementById('dmCallFloatGrip'),
        avatar: document.getElementById('dmCallFloatAvatar'),
        name: document.getElementById('dmCallFloatName'),
        status: document.getElementById('dmCallFloatStatus'),
        open: document.getElementById('dmCallFloatOpenBtn'),
        leave: document.getElementById('dmCallFloatLeaveBtn'),
        headerOpen: document.getElementById('dmActiveCallBtn'),
    };
}

function dmCallFloatCorner() {
    try {
        const raw = localStorage.getItem(DM_CALL_FLOAT_CORNER_KEY) || 'br';
        return ['tl', 'tr', 'bl', 'br'].includes(raw) ? raw : 'br';
    } catch (_) {
        return 'br';
    }
}

function setDmCallFloatCorner(corner) {
    const { root } = dmCallFloatEls();
    const safe = ['tl', 'tr', 'bl', 'br'].includes(corner) ? corner : 'br';
    if (root) {
        root.classList.remove('corner-tl', 'corner-tr', 'corner-bl', 'corner-br', 'is-dragging');
        root.classList.add(`corner-${safe}`);
        root.style.left = '';
        root.style.top = '';
        root.style.right = '';
        root.style.bottom = '';
    }
    try { localStorage.setItem(DM_CALL_FLOAT_CORNER_KEY, safe); } catch (_) {}
}

function dmCallMetaForChat(chatId) {
    const id = Number(chatId);
    const meta = dmMetaByChatId.get(id) || hiddenDmMeta.get(id) || {};
    const otherName = (meta.otherName || chatNameById.get(id) || 'DM').toString();
    const otherId = Number(meta.otherId || 0);
    const otherAvatarFileId = Number(meta.otherAvatarFileId || 0) || null;
    return { chatId: id, otherId, otherName };
}

function isKnownDmChat(chatId) {
    const id = Number(chatId);
    return Number.isFinite(id) && id > 0 && (
        dmMetaByChatId.has(id)
        || hiddenDmMeta.has(id)
        || (!currentServerId && Number(currentChatId || 0) === id)
    );
}

function openActiveDmCallChat() {
    if (!activeDmCall?.chatId) return;
    openDmChat(activeDmCall.chatId, activeDmCall.otherName || 'DM').catch((e) => console.warn('[DM CALL] open active DM failed', e));
}

function wireDmCallFloatOnce() {
    if (dmCallFloatWired) return;

    const { root, grip, open, leave, headerOpen } = dmCallFloatEls();
    if (!root) return;
    dmCallFloatWired = true;

    setDmCallFloatCorner(dmCallFloatCorner());

    open?.addEventListener('click', (e) => {
        e.preventDefault();
        e.stopPropagation();
        openActiveDmCallChat();
    });

    headerOpen?.addEventListener('click', (e) => {
        e.preventDefault();
        e.stopPropagation();
        openActiveDmCallChat();
    });

    leave?.addEventListener('click', (e) => {
        e.preventDefault();
        e.stopPropagation();
        try { window.lbVoice?.leave?.(); } catch (_) {}
    });

    let holdTimer = null;
    let dragging = false;
    let offsetX = 0;
    let offsetY = 0;

    const stopDrag = (ev) => {
        if (holdTimer) {
            clearTimeout(holdTimer);
            holdTimer = null;
        }
        if (!dragging) return;
        dragging = false;
        try { grip?.releasePointerCapture?.(ev.pointerId); } catch (_) {}
        const x = Number(ev.clientX || 0);
        const y = Number(ev.clientY || 0);
        const vertical = y < (window.innerHeight / 2) ? 't' : 'b';
        const horizontal = x < (window.innerWidth / 2) ? 'l' : 'r';
        setDmCallFloatCorner(`${vertical}${horizontal}`);
    };

    grip?.addEventListener('pointerdown', (ev) => {
        if (ev.button !== 0) return;
        if (ev.target?.closest?.('button')) return;
        const rect = root.getBoundingClientRect();
        offsetX = ev.clientX - rect.left;
        offsetY = ev.clientY - rect.top;
        holdTimer = setTimeout(() => {
            dragging = true;
            root.classList.add('is-dragging');
            try { grip.setPointerCapture?.(ev.pointerId); } catch (_) {}
        }, 170);
    });

    grip?.addEventListener('pointermove', (ev) => {
        if (!dragging) return;
        ev.preventDefault();
        const maxLeft = Math.max(8, window.innerWidth - root.offsetWidth - 8);
        const maxTop = Math.max(8, window.innerHeight - root.offsetHeight - 8);
        const left = Math.max(8, Math.min(maxLeft, ev.clientX - offsetX));
        const top = Math.max(8, Math.min(maxTop, ev.clientY - offsetY));
        root.classList.remove('corner-tl', 'corner-tr', 'corner-bl', 'corner-br');
        root.style.left = `${left}px`;
        root.style.top = `${top}px`;
        root.style.right = 'auto';
        root.style.bottom = 'auto';
    });

    grip?.addEventListener('pointerup', stopDrag);
    grip?.addEventListener('pointercancel', stopDrag);
}

function refreshDmCallFloat() {
    wireDmCallFloatOnce();
    const { root, avatar, name, status, headerOpen } = dmCallFloatEls();
    if (!root) return;

    const st = window.lbVoice?.getState?.();
    const chatId = Number(st?.channel_id || 0);
    const hasDmCall = Number.isFinite(chatId) && chatId > 0 && isKnownDmChat(chatId);

    if (!hasDmCall) {
        activeDmCall = null;
        root.hidden = true;
        if (headerOpen) headerOpen.hidden = true;
        document.body.classList.remove('dm-call-float-visible');
        return;
    }

    activeDmCall = dmCallMetaForChat(chatId);
    const display = activeDmCall.otherName || st?.channel_name || 'DM';
    const letter = (display.trim().charAt(0) || 'D').toUpperCase();
    const isCurrent = !currentServerId && Number(currentChatId || 0) === chatId;

    if (avatar) avatar.textContent = letter;
    if (name) name.textContent = display;
    if (status) status.textContent = isCurrent ? 'Звонок в этом DM' : 'DM-звонок';
    root.hidden = false;
    if (headerOpen) headerOpen.hidden = false;
    document.body.classList.add('dm-call-float-visible');
}

function onDmCallEvent(ev) {
    const t = (ev?.type || '').toString();
    if (!t.startsWith('dm_call_')) return;

    wireDmCallOverlayButtonsOnce();

    if (t === 'dm_call_missed') {
        const chatId = Number(ev.chat_id);
        const fromName = (ev.from_username || chatNameById.get(chatId) || 'DM').toString();
        const ts = Number(ev.timestamp || 0);
        const fresh = !Number.isFinite(ts) || ts <= 0 || (Date.now() - ts) < 10 * 60 * 1000;
        showInAppNotification({
            title: 'Пропущенный звонок',
            body: `${fromName} звонил вам`,
            tag: `dm-call-missed:${chatId}:${ts || ''}`,
            kind: 'call',
            timeout: fresh ? 9000 : 5500,
            onClick: () => {
                try { window.focus(); } catch (_) {}
                if (Number.isFinite(chatId) && chatId > 0) {
                    openDmChat(chatId, fromName).catch(() => {});
                }
            },
        });
        if (fresh) {
            showDesktopNotification('Пропущенный звонок', `${fromName} звонил вам`, `dm-call-missed:${chatId}:${ts || ''}`, {
                url: '/app',
                chatId,
            });
            playNotifySound();
        }
        return;
    }

    if (t === 'dm_call_invite') {
        const chatId = Number(ev.chat_id);
        const fromName = (ev.from_username || chatNameById.get(chatId) || 'DM').toString();
        showDmCallOverlay({
            chat_id: ev.chat_id,
            from_user_id: ev.from_user_id,
            from_username: ev.from_username
        });
        showInAppNotification({
            title: 'Входящий звонок',
            body: `${fromName} звонит вам`,
            tag: `dm-call:${chatId}`,
            kind: 'call',
            timeout: 12000,
            onClick: () => {
                try { window.focus(); } catch (_) {}
                if (Number.isFinite(chatId) && chatId > 0) {
                    openDmChat(chatId, fromName).catch(() => {});
                }
            },
        });
        showDesktopNotification('Входящий звонок', `${fromName} звонит вам`, `dm-call:${chatId}`, {
            url: '/app',
            chatId,
            requireInteraction: true,
        });
        playNotifySound();
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


function serverVisibilityLabel(isPublic) {
    return isPublic ? 'Публичный' : 'Приватный';
}

function getMyServersForRequest() {
    return (Array.isArray(lastServersSnapshot) ? lastServersSnapshot : [])
        .filter((s) => Number(s?.id) > 0)
        .map((s) => ({ id: Number(s.id), name: (s.name || `Сервер ${s.id}`).toString() }));
}

function serverHubModalHtml() {
    return `
      <div class="server-hub-modal modal" role="dialog" aria-modal="true">
        <div class="modal-header server-hub-head">
          <div>
            <h2>Добавить сервер</h2>
            <div class="muted">Создание, поиск и заявки в одном месте.</div>
          </div>
          <button class="icon-btn" type="button" data-act="close">✕</button>
        </div>
        <div class="server-hub-tabs" role="tablist">
          <button class="server-hub-tab active" type="button" data-tab="create">Создать</button>
          <button class="server-hub-tab" type="button" data-tab="search">Поиск</button>
          <button class="server-hub-tab" type="button" data-tab="requests">Заявки</button>
        </div>
        <div class="server-hub-body">
          <section class="server-hub-pane active" data-pane="create">
            <label class="form-label">Название сервера</label>
            <input class="inp" id="serverHubCreateName" maxlength="64" placeholder="Например: Global" autocomplete="off">
            <div class="server-visibility-pick" role="radiogroup" aria-label="Тип сервера">
              <label class="server-vis-card active">
                <input type="radio" name="serverHubVisibility" value="public" checked>
                <span class="server-vis-title">Публичный</span>
                <span class="server-vis-desc">Люди смогут найти сервер через поиск и зайти сразу.</span>
              </label>
              <label class="server-vis-card">
                <input type="radio" name="serverHubVisibility" value="private">
                <span class="server-vis-title">Приватный</span>
                <span class="server-vis-desc">Вход через заявку. Владелец/админ принимает или отклоняет.</span>
              </label>
            </div>
            <div class="server-hub-actions">
              <button class="btn btn-primary" type="button" data-act="create-server">Создать сервер</button>
            </div>
          </section>

          <section class="server-hub-pane" data-pane="search">
            <div class="server-search-line">
              <input class="inp" id="serverHubSearchInput" placeholder="Поиск публичных и приватных серверов..." autocomplete="off">
              <button class="btn" type="button" data-act="search-server">Найти</button>
            </div>
            <div class="server-hub-results" id="serverHubSearchResults">
              <div class="server-hub-empty">Введи название и нажми «Найти».</div>
            </div>
          </section>

          <section class="server-hub-pane" data-pane="requests">
            <div class="server-hub-request-tabs">
              <button class="server-hub-subtab active" type="button" data-req-tab="incoming">Входящие</button>
              <button class="server-hub-subtab" type="button" data-req-tab="outgoing">Мои заявки</button>
            </div>
            <div class="server-hub-results" id="serverHubRequestsList">
              <div class="server-hub-empty">Загрузка...</div>
            </div>
          </section>
        </div>
      </div>
    `;
}

async function openServerHubModal(initialTab = 'create') {
    const existing = document.getElementById('serverHubOverlay');
    if (existing) existing.remove();

    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay server-hub-overlay';
    overlay.id = 'serverHubOverlay';
    overlay.innerHTML = serverHubModalHtml();
    document.body.appendChild(overlay);

    const close = () => {
        try { overlay.remove(); } catch (_) {}
    };

    const setTab = (tab) => {
        overlay.querySelectorAll('.server-hub-tab').forEach((b) => b.classList.toggle('active', b.dataset.tab === tab));
        overlay.querySelectorAll('.server-hub-pane').forEach((p) => p.classList.toggle('active', p.dataset.pane === tab));
        if (tab === 'requests') loadServerHubRequests(overlay, 'incoming');
        if (tab === 'search') {
            const input = overlay.querySelector('#serverHubSearchInput');
            setTimeout(() => input?.focus(), 0);
        }
    };

        // Use a single event listener on the overlay for all interactions (Event Delegation)
    overlay.addEventListener('click', (e) => {
        const target = e.target;
        if (!(target instanceof Element)) return;

        if (target === overlay || target.closest('[data-act="close"]')) {
            e.preventDefault();
            close();
            return;
        }

        const tabBtn = target.closest('.server-hub-tab');
        if (tabBtn) {
            e.preventDefault();
            setTab(tabBtn.dataset.tab || 'create');
            return;
        }

        const subTab = target.closest('.server-hub-subtab');
        if (subTab) {
            e.preventDefault();

            const kind = subTab.dataset.reqTab || 'incoming';

            overlay
                .querySelectorAll('.server-hub-subtab')
                .forEach((btn) => {
                    btn.classList.toggle('active', btn === subTab);
                });

            loadServerHubRequests(overlay, kind);
            return;
        }

        const vis = target.closest('.server-vis-card');
        if (vis) {
            e.preventDefault();

            overlay
                .querySelectorAll('.server-vis-card')
                .forEach((card) => {
                    card.classList.toggle('active', card === vis);
                });

            const radio = vis.querySelector('input[type="radio"]');
            if (radio) radio.checked = true;

            return;
        }

        const actElement = target.closest('[data-act]');
        if (!actElement) return;

        const act = actElement.dataset.act;
        e.preventDefault();

        switch (act) {
            case 'create-server': {
                createServerFromHub(overlay, close);
                return;
            }

            case 'search-server': {
                searchServersFromHub(overlay);
                return;
            }

            case 'join-public': {
                const card = actElement.closest('[data-server-id]');
                if (card) joinServerFromHub(overlay, card);
                return;
            }

            case 'request-private': {
                const card = actElement.closest('[data-server-id]');
                if (card) requestServerFromHub(overlay, card);
                return;
            }

            case 'preview-server': {
                const card = actElement.closest('[data-server-id]');
                if (card) previewServerFromHub(card);
                return;
            }

            case 'accept-request': {
                const request = actElement.closest('[data-request-id]');
                if (request) decideJoinRequestFromHub(overlay, request, 'accept');
                return;
            }

            case 'reject-request': {
                const request = actElement.closest('[data-request-id]');
                if (request) decideJoinRequestFromHub(overlay, request, 'reject');
                return;
            }

            default:
                return;
        }
    });

    overlay.querySelector('#serverHubSearchInput')?.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') {
            e.preventDefault();
            searchServersFromHub(overlay);
        }
    });

    overlay.querySelector('#serverHubCreateName')?.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') {
            e.preventDefault();
            createServerFromHub(overlay, close);
        }
    });

    setTab(initialTab);
    if (initialTab === 'create') setTimeout(() => overlay.querySelector('#serverHubCreateName')?.focus(), 0);
}

async function createServerFromHub(overlay, close) {
    const input = overlay.querySelector('#serverHubCreateName');
    const name = (input?.value || '').trim();
    const visibility = overlay.querySelector('input[name="serverHubVisibility"]:checked')?.value || 'public';
    const isPublic = visibility !== 'private';

    if (!name) {
        showToast('Название сервера пустое');
        input?.focus();
        return;
    }

    try {
        const res = await api('/api/servers', {
            method: 'POST',
            body: JSON.stringify({ name, is_public: isPublic })
        });
        const servers = await api('/api/servers');
        renderServers(servers);
        const newId = Number(res?.id);
        const idToOpen = Number.isFinite(newId) && newId > 0 ? newId : (servers[0]?.id || null);
        const srv = servers.find(x => Number(x.id) === Number(idToOpen));
        if (idToOpen) await openServer(idToOpen, srv?.name || name);
        showToast(isPublic ? 'Публичный сервер создан' : 'Приватный сервер создан');
        close?.();
    } catch (e) {
        console.error('[UI] Failed to create server', e);
        showToast('Не удалось создать сервер');
    }
}

function serverHubCard(row) {
    const sid = Number(row?.id);
    const name = (row?.name || `Сервер ${sid}`).toString();
    const isPublic = row?.is_public !== false;
    const members = Number(row?.members_count || 0);
    const letter = (name.charAt(0) || 'S').toUpperCase();
    return `
      <div class="server-discover-card" data-server-id="${sid}" data-server-name="${escapeHtml(name)}" data-server-public="${isPublic ? '1' : '0'}" data-server-members="${members}">
        <div class="server-discover-avatar">${escapeHtml(letter)}</div>
        <div class="server-discover-main">
          <div class="server-discover-title">${escapeHtml(name)}</div>
          <div class="server-discover-meta">${serverVisibilityLabel(isPublic)} • участников: ${members}</div>
          <div class="server-discover-desc">${isPublic ? 'Можно присоединиться сразу.' : 'Вход через заявку владельцу/админам.'}</div>
        </div>
        <div class="server-discover-actions">
          <button class="btn btn-ghost" type="button" data-act="preview-server">Осмотр</button>
          ${isPublic
            ? `<button class="btn btn-primary" type="button" data-act="join-public">Войти</button>`
            : `<button class="btn btn-primary" type="button" data-act="request-private">Заявка</button>`}
        </div>
      </div>
    `;
}

async function searchServersFromHub(overlay) {
    const input = overlay.querySelector('#serverHubSearchInput');
    const box = overlay.querySelector('#serverHubSearchResults');
    const q = (input?.value || '').trim();
    if (!box) return;
    box.innerHTML = '<div class="server-hub-empty">Поиск...</div>';

    try {
        const rows = await api(`/api/servers/discover?q=${encodeURIComponent(q)}&limit=30`);
        const list = Array.isArray(rows) ? rows : [];
        box.innerHTML = list.length
            ? list.map(serverHubCard).join('')
            : '<div class="server-hub-empty">Ничего не найдено.</div>';
    } catch (e) {
        console.error('[UI] Failed to discover servers', e);
        box.innerHTML = '<div class="server-hub-empty bad">Не удалось выполнить поиск.</div>';
    }
}

async function joinServerFromHub(overlay, card) {
    const sid = Number(card?.dataset?.serverId);
    if (!Number.isFinite(sid) || sid <= 0) return;
    try {
        await api(`/api/servers/${sid}/join`, { method: 'POST' });
        const servers = await api('/api/servers');
        renderServers(servers);
        const srv = servers.find(x => Number(x.id) === sid);
        await openServer(sid, srv?.name || card?.dataset?.serverName || 'Сервер');
        showToast('Сервер добавлен');
        overlay?.remove?.();
    } catch (e) {
        console.error('[UI] Failed to join server', e);
        showToast('Не удалось войти на сервер');
    }
}

async function requestServerFromHub(overlay, card) {
    const sid = Number(card?.dataset?.serverId);
    if (!Number.isFinite(sid) || sid <= 0) return;
    try {
        await api(`/api/servers/${sid}/join-request`, {
            method: 'POST',
            body: JSON.stringify({})
        });
        showToast('Заявка отправлена');
        card?.classList?.add('is-requested');
    } catch (e) {
        console.error('[UI] Failed to request join', e);
        showToast('Не удалось отправить заявку');
    }
}

function previewServerFromHub(card) {
    if (!card) return;
    const name = card.dataset.serverName || 'Сервер';
    const isPublic = card.dataset.serverPublic === '1';
    const members = card.dataset.serverMembers || '0';
    alert(`${name}\n${serverVisibilityLabel(isPublic)}\nУчастников: ${members}`);
}

function joinRequestCard(row, mode) {
    const id = Number(row?.id);
    const serverName = (row?.server_name || 'Сервер').toString();
    const requester = (row?.requester_username || 'Пользователь').toString();
    const letter = (serverName.charAt(0) || 'S').toUpperCase();
    const status = (row?.status || 'pending').toString();
    const meta = mode === 'incoming'
        ? `Заявка от ${requester}`
        : `${serverVisibilityLabel(row?.server_is_public !== false)} • статус: ${status}`;

    return `
      <div class="server-discover-card request-card" data-request-id="${id}" data-server-id="${Number(row?.server_id)}" data-server-name="${escapeHtml(serverName)}">
        <div class="server-discover-avatar">${escapeHtml(letter)}</div>
        <div class="server-discover-main">
          <div class="server-discover-title">${escapeHtml(serverName)}</div>
          <div class="server-discover-meta">${escapeHtml(meta)}</div>
          <div class="server-discover-desc">${escapeHtml(row?.created_at || '')}</div>
        </div>
        <div class="server-discover-actions">
          <button class="btn btn-ghost" type="button" data-act="preview-server">Осмотр</button>
          ${mode === 'incoming' ? `
            <button class="btn btn-primary" type="button" data-act="accept-request">Принять</button>
            <button class="btn btn-danger" type="button" data-act="reject-request">Отклонить</button>
          ` : ''}
        </div>
      </div>
    `;
}

async function loadServerHubRequests(overlay, mode = 'incoming') {
    const box = overlay.querySelector('#serverHubRequestsList');
    if (!box) return;
    box.innerHTML = '<div class="server-hub-empty">Загрузка...</div>';

    try {
        const url = mode === 'outgoing' ? '/api/servers/join-requests/outgoing' : '/api/servers/join-requests/incoming';
        const rows = await api(url);
        const list = Array.isArray(rows) ? rows : [];
        box.innerHTML = list.length
            ? list.map((r) => joinRequestCard(r, mode)).join('')
            : `<div class="server-hub-empty">${mode === 'incoming' ? 'Входящих заявок нет.' : 'У тебя нет заявок.'}</div>`;
    } catch (e) {
        console.error('[UI] Failed to load join requests', e);
        box.innerHTML = '<div class="server-hub-empty bad">Не удалось загрузить заявки.</div>';
    }
}

async function decideJoinRequestFromHub(overlay, card, action) {
    const id = Number(card?.dataset?.requestId);
    if (!Number.isFinite(id) || id <= 0) return;
    try {
        await api(`/api/servers/join-requests/${id}/${action}`, { method: 'POST' });
        showToast(action === 'accept' ? 'Заявка принята' : 'Заявка отклонена');
        await loadServerHubRequests(overlay, 'incoming');
    } catch (e) {
        console.error('[UI] Failed to update join request', e);
        showToast('Не удалось обработать заявку');
    }
}

async function createServerFlow() {
    openServerHubModal('create');
}

const COOKIE_AGREEMENT_VERSION = 'cookies-geo-v1';

function needsCookieConsentPrompt() {
    const status = (currentUser?.cookie_consent_status || 'unknown').toString().toLowerCase();
    return status !== 'accepted' && status !== 'declined';
}

function removeCookieConsentModal() {
    document.getElementById('cookieConsentOverlay')?.remove();
}

function showCookieConsentModal() {
    removeCookieConsentModal();

    return new Promise((resolve) => {
        const overlay = document.createElement('div');
        overlay.id = 'cookieConsentOverlay';
        overlay.className = 'cookie-consent-overlay';
        overlay.setAttribute('role', 'dialog');
        overlay.setAttribute('aria-modal', 'true');
        overlay.innerHTML = `
            <div class="cookie-consent-card">
                <div class="cookie-consent-kicker">Безопасность LaBerry</div>
                <h2>Нужны cookies и проверочные данные</h2>
                <p>LaBerry использует необходимые cookies/storage для входа, сессии, настроек, E2EE-ключей в браузере и проверки правил доступа по локации.</p>
                <p>Сайт не может видеть список VPN-расширений браузера напрямую. Проверка VPN/proxy выполняется сервером по IP, CDN/proxy-заголовкам и другим сетевым сигналам.</p>
                <a class="cookie-consent-link" href="/cookie-agreement" target="_blank" rel="noopener">Открыть соглашение о cookies и проверке безопасности</a>
                <div class="cookie-consent-warning">Если отказаться, аккаунт получит низкий фактор доверия и попадёт в админ-панель на ручную проверку.</div>
                <div class="cookie-consent-actions">
                    <button class="btn btn-ghost" type="button" data-cookie-consent="decline">Отказаться</button>
                    <button class="btn btn-primary" type="button" data-cookie-consent="accept">Принять</button>
                </div>
            </div>
        `;
        document.body.appendChild(overlay);

        overlay.querySelectorAll('[data-cookie-consent]').forEach((btn) => {
            btn.addEventListener('click', async () => {
                const accepted = btn.getAttribute('data-cookie-consent') === 'accept';
                overlay.querySelectorAll('button').forEach((b) => { b.disabled = true; });
                try {
                    const res = await api('/api/users/me/cookie-consent', {
                        method: 'POST',
                        body: JSON.stringify({
                            accepted,
                            agreement_version: COOKIE_AGREEMENT_VERSION
                        })
                    });
                    currentUser = {
                        ...currentUser,
                        cookie_consent_status: res?.cookie_consent_status || (accepted ? 'accepted' : 'declined'),
                        trust_factor: Number(res?.trust_factor ?? (accepted ? 100 : 35)),
                        trust_review_status: res?.trust_review_status || (accepted ? 'clear' : 'review'),
                        trust_review_reason: res?.trust_review_reason || null
                    };
                    removeCookieConsentModal();
                    showToast(accepted ? 'Согласие сохранено' : 'Аккаунт отправлен на проверку доверия');
                    resolve(currentUser);
                } catch (e) {
                    console.warn('[COOKIE] consent save failed', e);
                    overlay.querySelectorAll('button').forEach((b) => { b.disabled = false; });
                    showToast('Не удалось сохранить выбор. Попробуйте ещё раз.');
                }
            });
        });
    });
}

async function ensureCookieConsentFlow() {
    if (!currentUser || !needsCookieConsentPrompt()) return;
    await showCookieConsentModal();
}

async function loadMe() {
    try {
        appLog("[UI] Loading current user...");
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
        appLog(`[ME] Loaded as ${currentUser.username}`);
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


function syncDrawerOverlayState() {
    const isOpen = document.body.classList.contains('channels-open') ||
        document.body.classList.contains('servers-open') ||
        document.body.classList.contains('members-open');
    document.body.classList.toggle('drawer-open', isOpen);
    const overlay = document.getElementById('uiOverlay');
    if (overlay) overlay.setAttribute('aria-hidden', isOpen ? 'false' : 'true');
}

function showChannelsMenu() {
    const channelsPanel = document.querySelector('.panel.channels');
    if (channelsPanel) {
        channelsPanel.classList.add('show-channels');
        document.body.classList.add('channels-open');
        hideServersMenu();
        hideMembersMenu();
        appLog('[UI] Channels menu shown');
        syncDrawerOverlayState();
    }
}

function hideChannelsMenu() {
    const channelsPanel = document.querySelector('.panel.channels');
    if (channelsPanel) {
        channelsPanel.classList.remove('show-channels');
        document.body.classList.remove('channels-open');
        appLog('[UI] Channels menu hidden');
        syncDrawerOverlayState();
    }
}

function toggleChannelsMenu() {
    const channelsPanel = document.querySelector('.panel.channels');
    if (channelsPanel) {
        const isVisible = channelsPanel.classList.contains('show-channels');
        appLog('[UI] Toggling channels menu, currently visible:', isVisible);
        if (isVisible) {
            hideChannelsMenu();
        } else {
            showChannelsMenu();
        }
    }
}

function isTouchUi() {
    try {
        return window.matchMedia('(pointer: coarse)').matches || window.innerWidth <= 900;
    } catch (_) {
        return window.innerWidth <= 900;
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
        syncDrawerOverlayState();
    }
}

function hideServersMenu() {
    const serversPanel = document.querySelector('.panel.servers');
    if (serversPanel) {
        serversPanel.classList.remove('show-servers');
        document.body.classList.remove('servers-open');
        syncDrawerOverlayState();
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
    if (!membersPanel) return;
    if (!canShowMembersMenu()) {
        hideMembersMenu();
        return;
    }
    membersPanel.hidden = false;
    membersPanel.classList.add('show-members');
    document.body.classList.add('members-open');
    hideChannelsMenu();
    hideServersMenu();
    syncDrawerOverlayState();
}

function hideMembersMenu() {
    const membersPanel = document.querySelector('.panel.members');
    if (membersPanel) {
        membersPanel.classList.remove('show-members');
        document.body.classList.remove('members-open');
        syncDrawerOverlayState();
    }
}

function toggleMembersMenu() {
    const membersPanel = document.querySelector('.panel.members');
    if (!membersPanel) return;
    if (!canShowMembersMenu()) {
        hideMembersMenu();
        return;
    }
    const isVisible = membersPanel.classList.contains('show-members');
    if (isVisible) hideMembersMenu();
    else showMembersMenu();
}

function isUtilityViewActive() {
    const utilityView = document.getElementById('utilityView');
    return !!utilityView && utilityView.hidden === false;
}

function isKnownDmChatId(chatId) {
    const cid = Number(chatId || 0);
    return Number.isFinite(cid) && cid > 0 && (dmMetaByChatId.has(cid) || hiddenDmMeta.has(cid));
}

function canShowMembersMenu() {
    if (isUtilityViewActive()) return false;
    if (!currentServerId && !isKnownDmChatId(currentChatId)) return false;
    return true;
}

function syncMobileMembersButton() {
    const btn = document.getElementById('mobileMembersBtn');
    if (!btn) return;
    btn.hidden = isUtilityViewActive() || (!currentServerId && !isKnownDmChatId(currentChatId));
}

function clearMembersPanelContent() {
    const membersPanel = document.getElementById('membersPanel');
    const membersList = document.getElementById('membersList');
    const countEl = membersPanel?.querySelector('.count');
    const titleEl = membersPanel?.querySelector('.panelHeader h3');
    if (membersList) membersList.innerHTML = '';
    if (countEl) countEl.textContent = '';
    if (titleEl) titleEl.textContent = 'Участники';
}

function isDmModeActive() {
    return !currentServerId && isKnownDmChatId(currentChatId);
}

function applyDmProfilePanelVisibility() {
    const membersPanel = document.getElementById('membersPanel');
    if (!membersPanel) return;
    const isDm = isDmModeActive();
    const hideOnDesktop = isDm && !isTouchUi() && dmProfilePanelHidden;
    if (isDm) {
        membersPanel.hidden = hideOnDesktop;
    }
    document.body.classList.toggle('dm-profile-panel-hidden', hideOnDesktop);
    syncDmProfilePanelButtons();
}

function setDmProfilePanelHidden(hidden) {
    dmProfilePanelHidden = !!hidden;
    try {
        localStorage.setItem(DM_PROFILE_PANEL_HIDDEN_KEY, dmProfilePanelHidden ? '1' : '0');
    } catch (_) {}
    applyDmProfilePanelVisibility();
}

function syncDmProfilePanelButtons() {
    const isDm = isDmModeActive();
    const showDesktopProfileControls = isDm && !isTouchUi();
    const toggleBtn = document.getElementById('profilePanelToggleBtn');
    const hideBtn = document.getElementById('membersPanelHideBtn');
    if (toggleBtn) {
        toggleBtn.hidden = !showDesktopProfileControls || !dmProfilePanelHidden;
        toggleBtn.title = dmProfilePanelHidden ? 'Показать профиль' : 'Профиль открыт';
    }
    if (hideBtn) {
        hideBtn.hidden = !showDesktopProfileControls || dmProfilePanelHidden;
    }
}


async function showMobileDmDrawer() {
    try {
        setUiModeDm();
        await loadDmList();
    } catch (e) {
        console.warn('[UI] Failed to open mobile DM drawer', e);
    }
    hideServersMenu();
    hideMembersMenu();
    showChannelsMenu();
}

async function showMobileServerChannelsDrawer(serverId, serverName) {
    if (serverId && Number(currentServerId || 0) !== Number(serverId)) {
        await openServer(serverId, serverName || 'Сервер');
    } else {
        setUiModeServer();
    }
    hideServersMenu();
    hideMembersMenu();
    showChannelsMenu();
}

function closeAllDrawers() {
    hideChannelsMenu();
    hideServersMenu();
    hideMembersMenu();
    syncDrawerOverlayState();
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
        const mode = emojiPickerEl.dataset.mode || 'reaction';
        if (mode === 'composer') {
            if (emoji) insertTextIntoComposer(emoji);
            hideEmojiPicker();
            return;
        }
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
    try { emojiPickerEl.dataset.mode = ''; } catch (_) {}
}

function insertTextIntoComposer(text) {
    const input = document.getElementById('message');
    const value = (text || '').toString();
    if (!input || !value) return;

    const current = input.value || '';
    const start = Number.isFinite(input.selectionStart) ? input.selectionStart : current.length;
    const end = Number.isFinite(input.selectionEnd) ? input.selectionEnd : start;
    input.value = current.slice(0, start) + value + current.slice(end);
    const next = start + value.length;
    try {
        input.selectionStart = next;
        input.selectionEnd = next;
    } catch (_) {}
    input.focus();
    input.dispatchEvent(new Event('input', { bubbles: true }));
}

function showEmojiPicker({ anchorEl, messageId, mode = 'reaction' } = {}) {
    ensureEmojiPicker();
    const mid = Number(messageId);
    const pickerMode = mode === 'composer' ? 'composer' : 'reaction';
    if (pickerMode !== 'composer' && (!Number.isFinite(mid) || mid <= 0)) return;
    if (!emojiPickerEl || !emojiPickerBackdrop) return;

    emojiPickerEl.dataset.mode = pickerMode;
    emojiPickerEl.dataset.forMsgId = pickerMode === 'composer' ? '' : String(mid);

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


function closeAnyServerMenu() {
    document.querySelectorAll('.server-menu-popover, .server-menu-backdrop').forEach((el) => el.remove());
}

function buildServerMenuButtonLabel(server) {
    return isServerMuted(server?.id) ? 'Включить уведомления' : 'Отключить уведомления';
}

function getServerSettingsGroups(server) {
    const name = (server?.name || 'seqjo ಥ_ಥ').toString();
    return [
        {
            title: `СЕРВЕР ${name}`,
            items: [
                ['profile', 'Профиль сервера'],
                ['tag', 'Тег сервера'],
                ['engagement', 'Вовлечённость'],
                ['boost', 'Бонусы буста'],
            ],
        },
        {
            title: 'РЕАКЦИИ',
            items: [
                ['emoji', 'Эмодзи'],
                ['stickers', 'Стикеры'],
                ['soundboard', 'Звуковая панель'],
            ],
        },
        {
            title: 'ЛЮДИ',
            items: [
                ['members', 'Участники'],
                ['roles', 'Роли'],
                ['invites', 'Приглашения'],
                ['access', 'Доступ'],
            ],
        },
        {
            title: 'ПРИЛОЖЕНИЯ',
            items: [
                ['integrations', 'Интеграция'],
                ['app-directory', 'Каталог приложений'],
            ],
        },
        {
            title: 'МОДЕРАЦИЯ',
            items: [
                ['safety', 'Настройка безопасности'],
                ['audit', 'Журнал аудита'],
                ['bans', 'Баны'],
                ['automod', 'Автомод'],
            ],
        },
        { title: '', items: [['community', 'Включить сообщество']] },
        { title: '', items: [['template', 'Шаблон сервера'], ['delete', 'Удалить сервер']] },
    ];
}

function renderServerSettingsContent(key, server) {
    const name = (server?.name || 'Сервер').toString();
    const letter = (name.trim().charAt(0) || 'S').toUpperCase();
    const created = formatProfileDate(server?.created_at);
    const publicLabel = server?.is_public === false ? 'Приватный' : 'Публичный';

    if (key === 'profile') {
        return `
          <div class="server-settings-page-grid">
            <section class="server-settings-main-section">
              <h2>Профиль сервера</h2>
              <p>Настройте отображение сервера в приглашениях, списках и карточках.</p>

              <label class="field server-settings-field">
                <span>Имя</span>
                <input class="inp" id="serverSettingsNameInput" value="${escapeHtml(name)}" maxlength="80" autocomplete="off" />
              </label>

              <div class="server-settings-divider"></div>

              <div class="server-settings-block-title">Значок</div>
              <div class="server-settings-note">Рекомендуется изображение 512x512. Сейчас доступна буквенная заглушка.</div>
              <button class="btn" type="button" data-act="server-icon-soon">Изменить значок сервера</button>

              <div class="server-settings-divider"></div>

              <div class="server-settings-block-title">Баннер</div>
              <div class="server-banner-swatches">
                ${['dark','pink','red','orange','yellow','purple','blue','mint','green','gray'].map((x) => `<button type="button" class="server-banner-swatch ${x}" data-banner="${x}" aria-label="${x}"></button>`).join('')}
              </div>

              <div class="server-settings-divider"></div>

              <div class="server-settings-block-title">Особенности</div>
              <div class="server-traits-grid">
                <input class="inp" placeholder="🙂" />
                <input class="inp" placeholder="🎮" />
                <input class="inp" placeholder="🎧" />
                <input class="inp" placeholder="💬" />
                <input class="inp" placeholder="👥" />
              </div>

              <div class="server-settings-footer-actions">
                <button class="btn btn-primary" type="button" data-act="save-profile">Сохранить</button>
              </div>
            </section>

            <aside class="server-profile-preview">
              <div class="server-profile-preview-banner"></div>
              <div class="server-profile-preview-icon">${escapeHtml(letter)}</div>
              <div class="server-profile-preview-name">${escapeHtml(name)}</div>
              <div class="server-profile-preview-meta"><span class="online-dot"></span> 1 в сети • ${escapeHtml(publicLabel)}</div>
              <div class="server-profile-preview-date">Дата основания: ${escapeHtml(created)}</div>
              <div class="server-profile-preview-badges"><span>VRC</span><span>osu!</span><span>★</span></div>
            </aside>
          </div>
        `;
    }

    const cardsByKey = {
        tag: ['Тег сервера', 'Короткая метка рядом с названием сервера. Под неё позже можно добавить проверку уникальности.'],
        engagement: ['Вовлечённость', 'Сводка активности, удержания участников и событий сервера.'],
        boost: ['Бонусы буста', 'Уровни буста, улучшения качества и видимые бонусы сообщества.'],
        emoji: ['Эмодзи', 'Глобальные и серверные эмодзи, лимиты и загрузка новых наборов.'],
        stickers: ['Стикеры', 'Наборы стикеров сервера и правила их публикации.'],
        soundboard: ['Звуковая панель', 'Короткие звуки для голосовых каналов и права на их запуск.'],
        members: ['Участники', 'Список участников, роли, дата входа и модераторские действия.'],
        roles: ['Роли', 'Иерархия ролей, цвета и права доступа к каналам.'],
        invites: ['Приглашения', 'Активные ссылки, срок действия и ограничения по использованию.'],
        access: ['Доступ', 'Права каналов, приватные зоны и правила видимости.'],
        integrations: ['Интеграция', 'Подключённые сервисы, боты, webhooks и внешние аккаунты.'],
        'app-directory': ['Каталог приложений', 'Подборка приложений, которые можно добавить на сервер.'],
        safety: ['Настройка безопасности', 'Фильтры входящих, защита от рейдов и базовые ограничения.'],
        audit: ['Журнал аудита', 'История административных действий: кто, что и когда изменил.'],
        bans: ['Баны', 'Список забаненных пользователей и причины блокировок.'],
        automod: ['Автомод', 'Фильтры ключевых слов, спама и массовых упоминаний.'],
        community: ['Включить сообщество', 'Переход к community-режиму, онбординг, правила и экран приветствия.'],
        template: ['Шаблон сервера', 'Создание шаблона с текущей структурой каналов и базовыми настройками.'],
    };

    if (key === 'delete') {
        return `
          <section class="server-settings-main-section danger-page">
            <h2>Удалить сервер</h2>
            <p>Это действие удалит каналы, сообщения, файлы и участников сервера.</p>
            <button class="btn btn-danger" type="button" data-act="delete-server">Удалить сервер</button>
          </section>
        `;
    }

    const [title, text] = cardsByKey[key] || ['Настройки', 'Раздел будет наполнен логикой позже.'];
    return `
      <section class="server-settings-main-section">
        <h2>${escapeHtml(title)}</h2>
        <p>${escapeHtml(text)}</p>
        <div class="server-settings-info-grid">
          <div class="server-settings-info-card"><b>Состояние</b><span>Заготовка интерфейса</span></div>
          <div class="server-settings-info-card"><b>Доступ</b><span>${canManageChannels(server?.id) ? 'Владелец сервера' : 'Только просмотр'}</span></div>
          <div class="server-settings-info-card"><b>Сервер</b><span>${escapeHtml(name)}</span></div>
        </div>
      </section>
    `;
}

function openServerSettingsModal(server) {
    closeAnyServerMenu();
    const sid = Number(server?.id);
    if (!Number.isFinite(sid) || sid <= 0) return;

    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay server-settings-overlay';
    const groups = getServerSettingsGroups(server);
    overlay.innerHTML = `
      <div class="server-settings-workspace" role="dialog" aria-modal="true">
        <aside class="server-settings-nav">
          ${groups.map((group) => `
            <div class="server-settings-nav-group">
              ${group.title ? `<div class="server-settings-nav-title">${escapeHtml(group.title)}</div>` : ''}
              ${group.items.map(([key, label]) => `
                <button class="server-settings-nav-item ${key === 'profile' ? 'active' : ''} ${key === 'delete' ? 'danger' : ''}" type="button" data-settings-tab="${escapeHtml(key)}">${escapeHtml(label)}</button>
              `).join('')}
            </div>
          `).join('')}
        </aside>
        <main class="server-settings-content">
          <div class="server-settings-content-inner" id="serverSettingsContent"></div>
        </main>
        <button class="server-settings-close" type="button" title="Закрыть"><span>✕</span><small>ESC</small></button>
      </div>`;

    const close = () => overlay.remove();
    overlay.querySelector('.server-settings-close')?.addEventListener('click', close);
    overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });

    let active = 'profile';
    const content = overlay.querySelector('#serverSettingsContent');
    const render = () => {
        if (!content) return;
        content.innerHTML = renderServerSettingsContent(active, server);
        wireServerSettingsContent();
    };

    const wireServerSettingsContent = () => {
        content?.querySelector('[data-act="save-profile"]')?.addEventListener('click', async () => {
            const nextName = (content.querySelector('#serverSettingsNameInput')?.value || '').toString().trim();
            if (!nextName) {
                showToast('Введите название сервера');
                return;
            }
            try {
                await api(`/api/servers/${sid}`, {
                    method: 'PATCH',
                    body: JSON.stringify({ name: nextName }),
                });
                server.name = nextName;
                lastServersSnapshot = (Array.isArray(lastServersSnapshot) ? lastServersSnapshot : []).map((s) => {
                    if (Number(s?.id) === sid) return { ...s, name: nextName };
                    return s;
                });
                renderServers(lastServersSnapshot);
                showToast('Профиль сервера сохранён');
                render();
            } catch (err) {
                console.warn('[UI] save server profile failed', err);
                showToast('Не удалось сохранить сервер');
            }
        });

        content?.querySelector('[data-act="server-icon-soon"]')?.addEventListener('click', () => {
            showToast('Загрузка значка сервера будет добавлена отдельно');
        });

        content?.querySelector('[data-act="delete-server"]')?.addEventListener('click', async () => {
            if (!confirm(`Удалить сервер «${(server?.name || 'сервер').toString()}»?`)) return;
            try {
                await api(`/api/servers/${sid}`, { method: 'DELETE' });
                window.location.reload();
            } catch (err) {
                console.warn('[UI] delete server failed', err);
                showToast('Не удалось удалить сервер');
            }
        });
    };

    overlay.querySelectorAll('[data-settings-tab]').forEach((btn) => {
        btn.addEventListener('click', () => {
            active = btn.dataset.settingsTab || 'profile';
            overlay.querySelectorAll('[data-settings-tab]').forEach((x) => x.classList.toggle('active', x === btn));
            render();
        });
    });

    document.body.appendChild(overlay);
    render();
}


function getCurrentServerForMenu() {
    const sid = Number(currentServerId);
    if (!Number.isFinite(sid) || sid <= 0) return null;
    const fromList = (Array.isArray(lastServersSnapshot) ? lastServersSnapshot : [])
        .find((s) => Number(s?.id) === sid);
    return fromList || { id: sid, name: 'Сервер' };
}

function openCurrentServerMenu(anchorEl) {
    const server = getCurrentServerForMenu();
    if (!server) return;
    openServerQuickMenu(server, anchorEl);
}

function openServerQuickMenu(server, anchorEl) {
    closeAnyServerMenu();
    const sid = Number(server?.id);
    if (!Number.isFinite(sid) || sid <= 0 || !anchorEl) return;
    const canManage = canManageChannels(sid);

    const backdrop = document.createElement('button');
    backdrop.type = 'button';
    backdrop.className = 'server-menu-backdrop';
    backdrop.setAttribute('aria-hidden', 'true');

    const pop = document.createElement('div');
    pop.className = 'server-menu-popover';
    const item = (label, icon, act) => `
      <button type="button" class="server-menu-item" data-act="${escapeHtml(act)}">
        <span>${escapeHtml(label)}</span><span class="server-menu-ic">${escapeHtml(icon)}</span>
      </button>`;
    pop.innerHTML = `
      ${item('Буст сервера', '◇', 'boost')}
      <div class="server-menu-divider"></div>
      ${item('Пригласить на сервер', '👥', 'invite')}
      ${canManage ? item('Настройки сервера', '⚙', 'settings') : ''}
      ${canManage ? item('Создать канал', '＋', 'create-channel') : ''}
      ${canManage ? item('Создать категорию', '▣', 'create-category') : ''}
      ${canManage ? item('Создать событие', '▤', 'create-event') : ''}
      ${item('Каталог приложений', '✦', 'app-directory')}
      <div class="server-menu-divider"></div>
      ${item('Параметры уведомлений', '🔔', 'toggle-notify')}
      ${item('Настройки конфиденциальности', '◇', 'privacy')}
      <div class="server-menu-divider"></div>
      ${item('Редактировать личный профиль', '✎', 'profile')}
      ${item('Скрыть заглушённые каналы', '□', 'hide-muted')}
      <div class="server-menu-divider"></div>
      ${item('Копировать ID сервера', 'ID', 'copy-id')}
    `;

    const rect = anchorEl.getBoundingClientRect();
    pop.style.top = `${Math.round(rect.bottom + 8)}px`;
    pop.style.left = `${Math.round(Math.max(12, rect.right - 260))}px`;

    backdrop.addEventListener('click', closeAnyServerMenu);
    pop.addEventListener('click', async (e) => {
        const btn = e.target?.closest?.('[data-act]');
        if (!btn) return;
        const act = btn.dataset.act;

        if (act === 'toggle-notify') {
            const nowMuted = toggleServerMuted(sid);
            closeAnyServerMenu();
            showToast(nowMuted ? 'Уведомления сервера отключены' : 'Уведомления сервера включены');
            return;
        }

        if (act === 'settings') {
            closeAnyServerMenu();
            openServerSettingsModal(server);
            return;
        }

        if (act === 'boost') {
            closeAnyServerMenu();
            openUtilityPanel('subscription', { mode: 'server' });
            return;
        }

        if (act === 'invite') {
            closeAnyServerMenu();
            const username = await askTextModal({
                title: 'Пригласить на сервер',
                label: 'Ник пользователя',
                placeholder: 'username',
                okText: 'Пригласить',
                cancelText: 'Отмена',
            });
            if (!username) return;
            try {
                await api(`/api/servers/${sid}/invite`, {
                    method: 'POST',
                    body: JSON.stringify({ username }),
                });
                showToast('Приглашение отправлено');
            } catch (err) {
                console.warn('[UI] invite failed', err);
                showToast('Не удалось отправить приглашение');
            }
            return;
        }

        if (act === 'create-channel') {
            closeAnyServerMenu();
            createChannelFlow();
            return;
        }

        if (act === 'app-directory') {
            closeAnyServerMenu();
            openUtilityPanel('store');
            return;
        }

        if (act === 'copy-id') {
            closeAnyServerMenu();
            try {
                await navigator.clipboard?.writeText?.(String(sid));
                showToast('ID сервера скопирован');
            } catch (_) {
                showToast(`ID сервера: ${sid}`);
            }
            return;
        }

        closeAnyServerMenu();
        const messages = {
            'create-category': 'Категории каналов будут добавлены отдельной моделью данных',
            'create-event': 'События сервера будут добавлены отдельным разделом',
            privacy: 'Настройки конфиденциальности сервера пока как заготовка',
            profile: 'Личный профиль на сервере будет подключён к профилю пользователя',
            'hide-muted': 'Скрытие заглушённых каналов будет добавлено после статусов каналов',
        };
        showToast(messages[act] || 'Раздел будет добавлен позже');
    });

    document.body.appendChild(backdrop);
    document.body.appendChild(pop);
}

function normalizeServerSearchText(value) {
    return (value ?? '')
        .toString()
        .trim()
        .toLowerCase()
        .replace(/\s+/g, ' ');
}

function serverMatchesSearch(server, query) {
    if (!query) return true;
    const name = normalizeServerSearchText(server?.name || '');
    const description = normalizeServerSearchText(server?.description || '');
    const id = normalizeServerSearchText(server?.id || '');
    return name.includes(query) || description.includes(query) || id.includes(query);
}

function setupServerSearch() {
    const input = document.getElementById('serverSearchInput');
    if (!input || input.dataset.wired === '1') return;

    input.dataset.wired = '1';
    serverSearchQuery = normalizeServerSearchText(input.value || '');

    input.addEventListener('input', () => {
        serverSearchQuery = normalizeServerSearchText(input.value || '');
        renderServers(lastServersSnapshot);
    });

    input.addEventListener('keydown', (e) => {
        if (e.key === 'Escape') {
            input.value = '';
            serverSearchQuery = '';
            renderServers(lastServersSnapshot);
            input.blur();
        }
    });
}


function renderServers(servers) {
    const sourceServers = Array.isArray(servers) ? servers : [];
    lastServersSnapshot = sourceServers;

    const searchInput = document.getElementById('serverSearchInput');
    if (searchInput) {
        serverSearchQuery = normalizeServerSearchText(searchInput.value || serverSearchQuery);
    }

    const query = normalizeServerSearchText(serverSearchQuery);
    const visibleServers = query
        ? sourceServers.filter((server) => serverMatchesSearch(server, query))
        : sourceServers;

    appLog('[DEBUG] renderServers called', {
        serversCount: sourceServers.length,
        visibleServersCount: visibleServers.length,
        currentServerId,
        serverSearchQuery: query,
        sessionServerId: sessionStorage.getItem("lastServerId")
    });

    const serversList = document.getElementById('servers-list');
    if (!serversList) {
        console.error('[ERROR] servers-list element not found!');
        return;
    }

    try { serverOwnerById.clear(); } catch (_) {}
    for (const server of sourceServers) {
        try { serverOwnerById.set(Number(server.id), Number(server.owner_id)); } catch (_) {}
    }

    serversList.innerHTML = '';

    if (!sourceServers.length) {
        serversList.innerHTML = `
            <div class="empty-servers">
                <p>Нет серверов</p>
                <button class="btn btn-ghost" id="addServerBtn">Создать сервер</button>
            </div>
        `;
        return;
    }

    if (!visibleServers.length) {
        serversList.innerHTML = `
            <div class="empty-servers">
                <p>Ничего не найдено</p>
            </div>
        `;
        return;
    }

    visibleServers.forEach(server => {
        const serverItem = document.createElement('div');
        const isActive = server.id === currentServerId;
        serverItem.className = `item server ${isActive ? 'active' : ''}`;
        serverItem.dataset.serverId = server.id;
        serverItem.dataset.testId = `server-${server.id}`;

        const serverDesc = (server.description || '').toString().trim();
        const serverMuted = isServerMuted(server.id);
        serverItem.innerHTML = `
            <div class="avatar">${(server.name || 'S')[0]?.toUpperCase() || 'S'}</div>
            <div class="text">
                <div class="title">${escapeHtml((server.name || '').toString())}</div>
                <div class="sub-row">
                  ${serverDesc ? `<div class="sub">${escapeHtml(serverDesc)}</div>` : `<div class="sub">Сервер</div>`}
                  ${serverMuted ? `<span class="server-muted-badge" title="Уведомления отключены">🔕</span>` : ''}
                </div>
            </div>
        `;

        serverItem.addEventListener('click', (e) => {
            e.stopPropagation();
            e.preventDefault();

            appLog('[CLICK] Server clicked:', {
                id: server.id,
                name: server.name,
                currentServerId,
                isActive
            });

            if (currentServerId === server.id) {
                appLog(`[UI] Server ${server.id} already active`);

                // if we are in Friends view, close it and ensure server UI is visible
                if (location.hash === '#/friends' && window.closeFriends) {
                    window.closeFriends();
                    try { history.replaceState(null, '', location.pathname + location.search); } catch (_) { location.hash = ''; }
                }

                serverItem.classList.add('refreshing');
                setTimeout(() => serverItem.classList.remove('refreshing'), 300);

                if (isTouchUi()) {
                    showMobileServerChannelsDrawer(server.id, server.name).catch((err) => console.warn('[UI] mobile channel drawer failed', err));
                }

                return;
            }

            appLog(`[UI] Opening server ${server.id} (${server.name})`);
            if (isTouchUi()) {
                showMobileServerChannelsDrawer(server.id, server.name).catch((err) => console.warn('[UI] mobile server open failed', err));
            } else {
                openServer(server.id, server.name);
            }
        });

        serversList.appendChild(serverItem);
    });


appLog('[DEBUG] Servers rendered:', serversList.children.length);
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
    const sid = Number(currentServerId);
    const show = Number.isFinite(sid) && sid > 0;
    btn.style.display = show ? '' : 'none';
    btn.textContent = '⚙';
    btn.title = 'Меню сервера';
    btn.setAttribute('aria-label', 'Меню сервера');
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
        const statusText = statusToLabel(st);

        el.className = `member status-${st} ${online ? 'online' : 'offline'}`;
        el.innerHTML = `
          <div class="avatar small">${avatarInnerHtml(m.avatar_file_id, m.username)}</div>
          <div class="text">
            <div class="member-name-row">
              <div class="name">${escapeHtml(m.username || 'Unknown')}</div>
              ${badgeHtml}
            </div>
            <div class="role">${escapeHtml(statusText)}</div>
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
        appLog('[UI] Server opening in progress, skipping');
        return;
    }
    
    isOpeningServer = true;
    
    try {
        appLog(`[UI] Opening server ${serverId} (${serverName})`);
        if ((location.hash === '#/friends' || location.hash === '#friends') && window.closeFriends) {
            window.closeFriends();
            // clear hash so friends doesn't reopen on refresh
            try { history.replaceState(null, '', location.pathname + location.search); } catch (_) { location.hash = ''; }
        }
        
        setUiModeServer();
        currentServerId = serverId;
        sessionStorage.setItem("lastServerId", serverId.toString());
        
        appLog('[DEBUG] State updated:', {
            currentServerId,
            sessionStorage: sessionStorage.getItem("lastServerId")
        });
        
        updateServerSelection(serverId);
        
        const chats = await api(`/api/servers/${serverId}/chats`);
        appLog(`[UI] Loaded ${chats.length} chats for server ${serverId}`);
        
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
        try { chatServerById.set(Number(chat.id), Number(chat?.server_id ?? currentServerId ?? 0)); } catch (_) {}

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
        const subText = (chat.description || '').toString().trim();

        const delBtn = canManage ? `<button class="channel-del" type="button" title="Удалить канал">🗑</button>` : '';

        channelItem.innerHTML = `
            <span class="hash">${icon}</span>
            <div class="text">
                <div class="title">
                  <span class="title-text">${escapeHtml(chat.name || '')}</span>
                  ${hasUnread ? `<span class="badge-unread" title="Непрочитано">${unread > 99 ? '99+' : unread}</span>` : ''}
                </div>
                ${subText ? `<div class="sub">${escapeHtml(subText)}</div>` : ''}
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
            if (isTouchUi()) closeAllDrawers();
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
  try { window.lbVoice?.syncDockVisibility?.(); } catch (_) {}
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
  try { window.lbVoice?.syncDockVisibility?.(); } catch (_) {}
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
  } finally {
    refreshDmCallFloat();
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
    refreshDmCallFloat();
  } catch (_) {}
});


function setUiModeServer() {
    const channelsPanel = document.getElementById('channelsPanel');
    const channelsTitle = channelsPanel?.querySelector('.panelHeader h3');
    const dmHomeMenu = document.getElementById('dmHomeMenu');
    const dmList = document.getElementById('dmList');
    const channelsList = document.getElementById('channels-list');
    const membersPanel = document.getElementById('membersPanel');
    const utilityView = document.getElementById('utilityView');

    if (channelsTitle) channelsTitle.textContent = 'Каналы';
    channelsPanel?.classList.remove('dm-mode');

    if (dmHomeMenu) dmHomeMenu.hidden = true;
    if (dmList) dmList.hidden = true;
    if (channelsList) channelsList.hidden = false;
    if (utilityView) utilityView.hidden = true;
    if (membersPanel) membersPanel.hidden = false;
    document.body.classList.remove('dm-profile-panel-hidden');
    syncDmProfilePanelButtons();
    syncMobileMembersButton();

    try { updateChannelAdminUi(); } catch (_) {}
}

function setUiModeDm() {
    const channelsPanel = document.getElementById('channelsPanel');
    const channelsTitle = channelsPanel?.querySelector('.panelHeader h3');
    const dmHomeMenu = document.getElementById('dmHomeMenu');
    const dmList = document.getElementById('dmList');
    const channelsList = document.getElementById('channels-list');
    const membersPanel = document.getElementById('membersPanel');

    if (channelsTitle) channelsTitle.textContent = 'Чаты';
    channelsPanel?.classList.add('dm-mode');

    if (channelsList) channelsList.hidden = true;
    if (dmHomeMenu) dmHomeMenu.hidden = false;
    if (dmList) dmList.hidden = false;
    const hasDmChat = isKnownDmChatId(currentChatId);
    // On desktop keep the DM profile visible unless the user collapsed it.
    if (membersPanel) membersPanel.hidden = isTouchUi() || !hasDmChat;
    if (hasDmChat) {
        try { renderDmProfile(currentChatId).catch(() => {}); } catch (_) {}
    } else {
        clearMembersPanelContent();
    }
    applyDmProfilePanelVisibility();
    syncMobileMembersButton();

    try { updateChannelAdminUi(); } catch (_) {}
}

function setDmHomeActive(tab) {
    const key = (tab || '').toString();
    document.querySelectorAll('.dm-home-item').forEach((btn) => {
        btn.classList.toggle('active', btn.dataset.dmTab === key);
    });
}

function hideUtilityView() {
    const utilityView = document.getElementById('utilityView');
    if (utilityView) utilityView.hidden = true;
}

function utilityPlanCard(name, price, tone, lines) {
    const list = (Array.isArray(lines) ? lines : [])
        .map((line) => `<li>${escapeHtml(line)}</li>`)
        .join('');
    return `
      <button class="subscription-plan ${tone || ''}" type="button" data-plan="${escapeHtml(name)}">
        <span class="subscription-plan-name">${escapeHtml(name)}</span>
        <span class="subscription-plan-price">${escapeHtml(price)}</span>
        <ul>${list}</ul>
      </button>
    `;
}

function formatDownloadSize(size) {
    const n = Number(size || 0);
    if (!Number.isFinite(n) || n <= 0) return '';
    if (n >= 1024 * 1024 * 1024) return `${(n / (1024 * 1024 * 1024)).toFixed(1)} ГБ`;
    if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} МБ`;
    if (n >= 1024) return `${(n / 1024).toFixed(1)} КБ`;
    return `${n} Б`;
}

function renderDownloadsUtility(items = null) {
    const list = Array.isArray(items) ? items : [];
    const byPlatform = new Map(list.map((item) => [item?.platform, item]));
    const cards = ['android', 'pc'].map((platform) => {
        const item = byPlatform.get(platform) || {};
        const title = item.title || (platform === 'android' ? 'Мобильная версия' : 'ПК клиент');
        const sub = item.available
            ? `${item.version ? `Версия ${item.version}` : 'Сборка доступна'}${item.file_size ? ` • ${formatDownloadSize(item.file_size)}` : ''}`
            : 'Сборка ещё не загружена в админ-панели';
        return `
          <div class="download-card ${item.available ? '' : 'disabled'}">
            <div class="download-card-ic">${platform === 'android' ? 'APK' : 'PC'}</div>
            <div class="download-card-main">
              <h3>${escapeHtml(title)}</h3>
              <p>${escapeHtml(sub)}</p>
              ${item.original_name ? `<div class="download-file-name">${escapeHtml(item.original_name)}</div>` : ''}
            </div>
            ${item.available && item.download_url
                ? `<a class="btn btn-primary" href="${escapeHtml(item.download_url)}" download>Скачать</a>`
                : '<button class="btn" type="button" disabled>Недоступно</button>'}
          </div>
        `;
    }).join('');

    return `
      <section class="utility-shell downloads-shell">
        <div class="utility-hero compact">
          <div>
            <div class="utility-kicker">Загрузки</div>
            <h2>Скачать LaBerry</h2>
            <p>Выберите мобильную версию или ПК клиент. Файлы публикуются из админ-панели и отдаются сервером.</p>
          </div>
        </div>
        <div class="download-list">${items === null ? '<div class="muted">Загрузка...</div>' : cards}</div>
      </section>
    `;
}

async function loadDownloadsUtility(utilityView) {
    try {
        const items = await api('/api/downloads/');
        utilityView.innerHTML = renderDownloadsUtility(items);
    } catch (e) {
        console.warn('[DOWNLOADS] load failed', e);
        utilityView.innerHTML = renderDownloadsUtility([]);
        showToast('Не удалось загрузить список сборок');
    }
}

function subscriptionServerOptionsHtml() {
    const servers = (Array.isArray(lastServersSnapshot) ? lastServersSnapshot : [])
        .filter((server) => Number(server?.id) > 0);

    if (!servers.length) {
        return '<option value="">Нет доступных серверов</option>';
    }

    return servers
        .map((server) => `<option value="${Number(server.id)}">${escapeHtml(server.name || `Сервер ${server.id}`)}</option>`)
        .join('');
}

function renderSubscriptionUtility(mode = 'personal') {
    const isServer = mode === 'server';
    const checkoutBody = isServer
        ? `
            <label class="subscription-field">
              <span>Сервер для поддержки</span>
              <select class="inp" id="subscriptionServerSelect">
                ${subscriptionServerOptionsHtml()}
              </select>
            </label>
            <button class="btn btn-primary" type="button" id="subscriptionPayBtn">Поддержать</button>
          `
        : `
            <label class="subscription-option"><input type="radio" name="subTarget" value="self" checked /> <span>Купить себе</span></label>
            <label class="subscription-option"><input type="radio" name="subTarget" value="gift" /> <span>Подарить подписку</span></label>
            <input class="inp" id="subscriptionGiftInput" placeholder="Ник получателя подарка" autocomplete="off" hidden />
            <button class="btn btn-primary" type="button" id="subscriptionPayBtn">Перейти к оплате</button>
          `;
    return `
      <section class="utility-shell subscription-shell">
        <div class="utility-hero">
          <div>
            <div class="utility-kicker">Подписка</div>
            <h2>${isServer ? 'Поддержать сервер' : 'Личная подписка'}</h2>
            <p>${isServer
                ? 'Помощь серверу, бусты и видимые бонусы для сообщества будут подключаться через платежный backend.'
                : 'Выберите план для себя или подготовьте подарок другому пользователю.'}</p>
          </div>
          <div class="subscription-switch">
            <button class="${isServer ? 'active' : ''}" type="button" data-sub-mode="server">Поддержать сервер</button>
            <button class="${!isServer ? 'active' : ''}" type="button" data-sub-mode="personal">Личная подписка</button>
          </div>
        </div>

        <div class="subscription-layout">
          <div class="subscription-plans">
            ${utilityPlanCard('Berry Lite', '149 ₽ / мес', '', ['Расширенные реакции', 'Акцент профиля', 'Бейдж подписчика'])}
            ${utilityPlanCard('Berry Plus', '299 ₽ / мес', 'featured', ['GIF-избранное без лимита', 'Подарочные месяцы', 'Приоритетные функции'])}
            ${utilityPlanCard('Berry Ultra', '599 ₽ / мес', '', ['Буст сервера', 'Расширенные темы', 'Ранний доступ'])}
          </div>

          <div class="subscription-checkout-card">
            <div class="subscription-checkout-title">${isServer ? 'Поддержка сервера' : 'Оформление'}</div>
            ${checkoutBody}
            <div class="subscription-payment-methods">
              <label class="subscription-option"><input type="radio" name="paymentMethod" value="qr" checked /> <span>Оплата по QR-Code</span></label>
              <label class="subscription-option"><input type="radio" name="paymentMethod" value="card" /> <span>Карта через провайдера</span></label>
            </div>
            <div class="subscription-qr-box" id="subscriptionQrBox">
              <div class="subscription-qr-mark">QR</div>
              <span>Безопасный сценарий: код открывает платёжную страницу провайдера, данные карты не попадают в мессенджер.</span>
            </div>
            <label class="subscription-save-pay">
              <input type="checkbox" id="subscriptionSavePayment" />
              <span>Оставить платёжные данные в браузере</span>
            </label>
            <div class="subscription-danger-note" id="subscriptionPaymentWarning" hidden>
              Осторожно: сейчас сервер не имеет полноценной защиты для хранения платёжных данных. При входе в ваш аккаунт данные могут быть украдены. Безопаснее использовать QR-Code и не сохранять карту.
            </div>
            <div class="subscription-note">Сейчас это интерфейсная заготовка. Реальное списание нужно подключать через платежного провайдера и webhook-подтверждение.</div>
          </div>
        </div>
      </section>
    `;
}

function renderStoreUtility() {
    return `
      <section class="utility-shell">
        <div class="utility-hero compact">
          <div>
            <div class="utility-kicker">Магазин</div>
            <h2>Оформление, GIF и бонусы</h2>
            <p>Место для визуальных тем, наборов GIF, бейджей и серверных улучшений.</p>
          </div>
        </div>
        <div class="utility-grid">
          <div class="utility-card"><div class="utility-card-ic">GIF</div><h3>Наборы GIF</h3><p>Глобальные коллекции и личные подборки.</p></div>
          <div class="utility-card"><div class="utility-card-ic">★</div><h3>Бейджи</h3><p>Видимые отметки профиля без лишней нагрузки на чат.</p></div>
          <div class="utility-card"><div class="utility-card-ic">▣</div><h3>Темы</h3><p>Аккуратные темы интерфейса для тёмного режима.</p></div>
        </div>
      </section>
    `;
}

function renderQuestsUtility() {
    return `
      <section class="utility-shell">
        <div class="utility-hero compact">
          <div>
            <div class="utility-kicker">Задания</div>
            <h2>Активности и награды</h2>
            <p>Ежедневные задачи, приглашения друзей и серверные цели можно будет подключить к системе наград.</p>
          </div>
        </div>
        <div class="quests-list">
          <div class="quest-row"><span>01</span><div><b>Отправить первое сообщение</b><small>Награда: 10 ягод</small></div><button class="btn" type="button">В процессе</button></div>
          <div class="quest-row"><span>02</span><div><b>Добавить GIF в избранное</b><small>Награда: бейдж активности</small></div><button class="btn" type="button">Открыть</button></div>
          <div class="quest-row"><span>03</span><div><b>Пригласить друга</b><small>Награда: бонус профиля</small></div><button class="btn" type="button">Открыть</button></div>
        </div>
      </section>
    `;
}

function openUtilityPanel(tab, opts = {}) {
    const utilityView = document.getElementById('utilityView');
    const chatView = document.getElementById('chatView');
    const friendsView = document.getElementById('friendsView');
    const membersPanel = document.getElementById('membersPanel');
    if (!utilityView) return;

    currentServerId = null;
    updateServerSelection(null);
    closeAnyServerMenu();
    if (typeof window.closeFriends === 'function') {
        try { window.closeFriends({ clearHash: true }); } catch (_) {}
    }
    setUiModeDm();
    setDmHomeActive(tab);

    if (chatView) chatView.hidden = true;
    if (friendsView) friendsView.hidden = true;
    hideMembersMenu();
    if (membersPanel) membersPanel.hidden = true;
    clearMembersPanelContent();
    utilityView.hidden = false;
    syncMobileMembersButton();

    if (tab === 'home') {
        window.location.href = '/start';
        return;
    } else if (tab === 'downloads') {
        utilityView.innerHTML = renderDownloadsUtility(null);
        loadDownloadsUtility(utilityView).catch(() => {});
    } else if (tab === 'subscription') {
        utilityView.innerHTML = renderSubscriptionUtility(opts.mode || 'personal');
        utilityView.querySelectorAll('[data-sub-mode]')?.forEach?.((btn) => {
            btn.addEventListener('click', () => {
                openUtilityPanel('subscription', { mode: btn.dataset.subMode || 'personal' });
            });
        });
        utilityView.querySelectorAll('.subscription-plan').forEach((btn) => {
            btn.addEventListener('click', () => {
                utilityView.querySelectorAll('.subscription-plan').forEach((x) => x.classList.remove('selected'));
                btn.classList.add('selected');
            });
        });
        const syncGiftInput = () => {
            const target = utilityView.querySelector('input[name="subTarget"]:checked')?.value || 'self';
            const giftInput = utilityView.querySelector('#subscriptionGiftInput');
            if (giftInput) {
                giftInput.hidden = target !== 'gift';
                if (target !== 'gift') giftInput.value = '';
            }
        };
        utilityView.querySelectorAll('input[name="subTarget"]').forEach((radio) => {
            radio.addEventListener('change', syncGiftInput);
        });
        syncGiftInput();
        const syncPaymentStorageWarning = () => {
            const warning = utilityView.querySelector('#subscriptionPaymentWarning');
            const save = utilityView.querySelector('#subscriptionSavePayment');
            if (warning) warning.hidden = !save?.checked;
        };
        utilityView.querySelector('#subscriptionSavePayment')?.addEventListener('change', syncPaymentStorageWarning);
        syncPaymentStorageWarning();
        utilityView.querySelector('#subscriptionPayBtn')?.addEventListener('click', () => {
            showToast('Платежный backend пока не подключен');
        });
    } else if (tab === 'store') {
        utilityView.innerHTML = renderStoreUtility();
    } else {
        utilityView.innerHTML = renderQuestsUtility();
    }
}

function openDmHomeTab(tab) {
    const key = (tab || 'friends').toString();
    if (key === 'friends') {
        hideUtilityView();
        setDmHomeActive('friends');
        if (typeof window.openFriends === 'function') {
            try { window.openFriends(); return; } catch (_) {}
        }
        location.hash = '#/friends';
        return;
    }
    openUtilityPanel(key);
}

async function openCreateGroupChatModal() {
    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay';
    overlay.innerHTML = `
      <div class="modal group-chat-modal" role="dialog" aria-modal="true">
        <div class="modal-header">
          <div class="modal-title">Создать групповой чат</div>
          <button class="modal-close" type="button">✕</button>
        </div>
        <div class="modal-body group-chat-body">
          <label class="field">
            <span>Название</span>
            <input class="inp" id="groupChatNameInput" placeholder="Например: игровой вечер" autocomplete="off" />
          </label>
          <label class="field">
            <span>Участники</span>
            <input class="inp" id="groupChatSearchInput" placeholder="Поиск по друзьям..." autocomplete="off" />
          </label>
          <div class="group-chat-picker" id="groupChatPicker">
            <div class="muted" style="padding:12px;">Загрузка друзей...</div>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn" type="button" data-act="cancel">Отмена</button>
          <button class="btn btn-primary" type="button" data-act="create" disabled>Создать</button>
        </div>
      </div>
    `;

    const close = () => overlay.remove();
    overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });
    overlay.querySelector('.modal-close')?.addEventListener('click', close);
    overlay.querySelector('[data-act="cancel"]')?.addEventListener('click', close);
    document.body.appendChild(overlay);

    const picker = overlay.querySelector('#groupChatPicker');
    const search = overlay.querySelector('#groupChatSearchInput');
    const createBtn = overlay.querySelector('[data-act="create"]');
    const selected = new Set();
    let friends = [];

    const syncCreate = () => {
        if (createBtn) createBtn.disabled = selected.size < 2;
    };

    const renderFriends = () => {
        const q = (search?.value || '').toString().trim().toLowerCase();
        const visible = friends.filter((u) => (u?.username || '').toString().toLowerCase().includes(q));
        if (!picker) return;
        if (!visible.length) {
            picker.innerHTML = `<div class="muted" style="padding:12px;">Нет подходящих друзей</div>`;
            return;
        }
        picker.innerHTML = visible.map((u) => {
            const id = Number(u?.id || 0);
            const name = (u?.username || 'Unknown').toString();
            const checked = selected.has(id) ? 'checked' : '';
            return `
              <label class="group-chat-user">
                <input type="checkbox" value="${id}" ${checked} />
                <span class="avatar small">${escapeHtml((name.charAt(0) || 'U').toUpperCase())}</span>
                <span class="group-chat-user-main">
                  <span>${escapeHtml(name)}</span>
                  <small>${escapeHtml(statusToLabel(u?.status || (u?.is_online ? 'online' : 'offline')))}</small>
                </span>
              </label>
            `;
        }).join('');

        picker.querySelectorAll('input[type="checkbox"]').forEach((box) => {
            box.addEventListener('change', () => {
                const id = Number(box.value);
                if (!Number.isFinite(id) || id <= 0) return;
                if (box.checked) selected.add(id);
                else selected.delete(id);
                syncCreate();
            });
        });
    };

    try {
        friends = await api('/api/friends');
        if (!Array.isArray(friends)) friends = [];
        renderFriends();
    } catch (e) {
        console.warn('[DM] friends load failed for group modal', e);
        if (picker) picker.innerHTML = `<div class="muted" style="padding:12px;">Не удалось загрузить друзей</div>`;
    }

    search?.addEventListener('input', renderFriends);
    createBtn?.addEventListener('click', async () => {
        if (selected.size < 2) return;
        const name = (overlay.querySelector('#groupChatNameInput')?.value || '').toString().trim();
        createBtn.disabled = true;
        try {
            const res = await api('/api/dms/groups', {
                method: 'POST',
                body: JSON.stringify({ name: name || null, user_ids: [...selected] }),
            });
            close();
            await loadDmList();
            await openDmChat(Number(res?.chat_id), (res?.title || name || 'Групповой чат').toString());
        } catch (e) {
            console.warn('[DM] group create failed', e);
            showToast('Не удалось создать групповой чат');
            createBtn.disabled = false;
        }
    });
    syncCreate();
}

function setupDmHomeMenu() {
    const menu = document.getElementById('dmHomeMenu');
    if (!menu || menu.dataset.wired === '1') return;
    menu.dataset.wired = '1';

    menu.querySelectorAll('.dm-home-item').forEach((btn) => {
        btn.addEventListener('click', () => openDmHomeTab(btn.dataset.dmTab || 'friends'));
    });

    document.getElementById('dmFindBtn')?.addEventListener('click', () => openCreateGroupChatModal());
    document.getElementById('createGroupChatBtn')?.addEventListener('click', () => openCreateGroupChatModal());
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
    if (e2eeIsEncryptedText(raw)) return '🔒 Зашифрованное сообщение';
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
        if (e2eeIsEncryptedText(raw)) return '🔒 Зашифрованное сообщение';
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
                isGroup: !!v.isGroup,
                memberCount: Number(v.memberCount || 0) || 0,
                memberNames: Array.isArray(v.memberNames) ? v.memberNames : [],
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
                isGroup: !!meta?.isGroup,
                memberCount: Number(meta?.memberCount || 0) || 0,
                memberNames: Array.isArray(meta?.memberNames) ? meta.memberNames : [],
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
            isGroup: !!meta.isGroup,
            memberCount: Number(meta.memberCount || 0) || 0,
            memberNames: Array.isArray(meta.memberNames) ? meta.memberNames : [],
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
        if (!currentServerId && currentChatId) {
            try { renderDmProfile(currentChatId).catch(() => {}); } catch (_) {}
        }
        refreshDmCallFloat();
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
        const otherAvatarFileId = Number(dm?.other_avatar_file_id || 0) || null;
        const memberCount = Number(dm?.member_count || 0) || 0;
        const isGroup = !!dm?.is_group || memberCount > 2;
        const memberNames = Array.isArray(dm?.member_names) ? dm.member_names.map((x) => (x || '').toString()).filter(Boolean) : [];
        const otherName = (isGroup ? (dm?.title || dm?.other_username || 'Групповой чат') : (dm?.other_username || 'Unknown')).toString();
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
            dmMetaByChatId.set(chatId, { otherId, otherName, otherAvatarFileId, isGroup, memberCount, memberNames });
            try { chatKindById.set(chatId, 'dm'); } catch (_) {}
        }

        if (hiddenDmChats.has(chatId)) {
            // keep meta for auto-unhide
            if (Number.isFinite(chatId) && chatId > 0) {
                hiddenDmMeta.set(chatId, { otherId, otherName, otherAvatarFileId, isGroup, memberCount, memberNames });
                saveHiddenDmMeta();
            }
            continue;
        }

        item.className = `item dm ${(!currentServerId && currentChatId === chatId) ? 'active' : ''}`;
        item.dataset.chatId = String(chatId);
        item.dataset.otherUserId = String(otherId);
        item.dataset.group = isGroup ? '1' : '0';

        const letter = (otherName.charAt(0) || 'U').toUpperCase();
        const subText = isGroup
            ? `${memberCount || memberNames.length || 1} участника${preview ? ` • ${preview}` : ''}`
            : (preview || 'Личное сообщение');

        item.innerHTML = `
            <div class="avatar ${isGroup ? 'dm-group-avatar' : ''}">${isGroup ? '👥' : escapeHtml(letter)}</div>
            <div class="text">
                <div class="title">${escapeHtml(otherName)}</div>
                <div class="sub">${escapeHtml(subText)}</div>
            </div>
            <button class="dm-hide" type="button" title="Скрыть чат">✕</button>
        `;

        if (!isGroup) {
            const avatarEl = item.querySelector('.avatar');
            if (avatarEl) avatarEl.innerHTML = avatarInnerHtml(otherAvatarFileId, otherName);
        }

        item.querySelector('.dm-hide')?.addEventListener('click', (e) => {
            e.preventDefault();
            e.stopPropagation();
            hideDmChat(chatId, { otherId, otherName, otherAvatarFileId, isGroup, memberCount, memberNames });
        });

        item.addEventListener('click', () => {
            openDmChat(chatId, otherName).catch((e) => console.warn('[UI] openDmChat failed', e));
            if (isTouchUi()) closeAllDrawers();
        });

        dmList.appendChild(item);
    }

    wireAvatarFallbacks(dmList);
}

function renderDmMembers(chatId) {
    const membersPanel = document.getElementById('membersPanel');
    const membersWrap = document.getElementById('membersPanelMembers');
    const membersList = document.getElementById('membersList');
    const countEl = membersPanel?.querySelector('.count');
    const titleEl = membersPanel?.querySelector('.panelHeader h3');
    if (!membersPanel || !membersList) return;

    const cid = Number(chatId);
    if (!isKnownDmChatId(cid)) {
        clearMembersPanelContent();
        membersPanel.hidden = true;
        return;
    }
    const meta = dmMetaByChatId.get(cid) || hiddenDmMeta.get(cid) || {};
    const otherName = (meta.otherName || chatNameById.get(cid) || 'Собеседник').toString();
    const otherId = Number(meta.otherId || 0);
    const meName = (currentUser?.username || 'Вы').toString();

    if (titleEl) titleEl.innerHTML = `Участники <span class="count">(2)</span>`;
    if (countEl) countEl.textContent = '(2)';
    if (membersWrap) membersWrap.hidden = false;
    membersPanel.hidden = isTouchUi();

    membersList.innerHTML = '';
    const block = document.createElement('div');
    block.className = 'members-group-box dm-members-box';

    const makeRow = ({ id, username, avatar_file_id, role, clickable = false }) => {
        const el = document.createElement('div');
        el.className = 'member dm-member online';
        el.innerHTML = `
          <div class="avatar small">${avatarInnerHtml(avatar_file_id, username)}</div>
          <div class="text">
            <div class="name">${escapeHtml(username)}</div>
            <div class="role">${escapeHtml(role)}</div>
          </div>`;
        if (clickable && Number.isFinite(Number(id)) && Number(id) > 0) {
            el.dataset.userId = String(id);
            el.dataset.username = username;
            el.addEventListener('click', (e) => {
                e.stopPropagation();
                showUserMenu({
                    userId: Number(id),
                    username,
                    anchorEl: e?.target?.closest?.('.avatar') || el,
                    allowDm: true,
                    allowAddFriend: true,
                    allowRemoveFriend: false,
                });
            });
        }
        return el;
    };

    block.appendChild(makeRow({
        id: currentUser?.id,
        username: meName,
        avatar_file_id: currentUserProfile?.avatar_file_id,
        role: 'Вы',
        clickable: false,
    }));

    block.appendChild(makeRow({
        id: otherId,
        username: otherName,
        avatar_file_id: otherAvatarFileId,
        role: 'Личные сообщения',
        clickable: true,
    }));

    membersList.appendChild(block);
}

function dmConnectionBadge(kind) {
    const k = (kind || 'link').toString().toLowerCase();
    if (k.includes('github')) return 'GH';
    if (k.includes('youtube')) return 'YT';
    if (k.includes('telegram')) return 'TG';
    if (k.includes('discord')) return 'DC';
    if (k.includes('twitch')) return 'TW';
    return 'URL';
}

function dmProfileConnectionsHtml(connections) {
    const items = Array.isArray(connections) ? connections : [];
    const html = items
        .map((item) => {
            const rawUrl = (item?.url || item?.href || '').toString().trim();
            let href = '';
            try {
                const parsed = new URL(rawUrl);
                if (parsed.protocol === 'http:' || parsed.protocol === 'https:') href = parsed.href;
            } catch (_) {}
            if (!href) return '';

            const kind = (item?.kind || item?.provider || 'link').toString();
            const label = (item?.label || item?.title || kind || 'Ссылка').toString();
            const host = (() => {
                try { return new URL(href).hostname.replace(/^www\./, ''); }
                catch (_) { return href.replace(/^https?:\/\//i, ''); }
            })();

            return `
              <a class="profile-connection dm-profile-connection" href="${escapeHtml(href)}" target="_blank" rel="noopener noreferrer">
                <span class="profile-connection-kind">${escapeHtml(dmConnectionBadge(kind))}</span>
                <span class="profile-connection-main">
                  <span class="profile-connection-label">${escapeHtml(label)}</span>
                  <span class="profile-connection-url">${escapeHtml(host)}</span>
                </span>
              </a>
            `;
        })
        .filter(Boolean);

    return html.length
        ? `<div class="profile-connections">${html.join('')}</div>`
        : '<div class="profile-text empty">Интеграции не добавлены</div>';
}

async function renderGroupDmPanel(chatId, meta = {}) {
    const membersPanel = document.getElementById('membersPanel');
    const membersWrap = document.getElementById('membersPanelMembers');
    const membersList = document.getElementById('membersList');
    const countEl = membersPanel?.querySelector('.count');
    const titleEl = membersPanel?.querySelector('.panelHeader h3');
    if (!membersPanel || !membersList) return;

    const cid = Number(chatId);
    const title = (meta.otherName || chatNameById.get(cid) || 'Групповой чат').toString();

    if (titleEl) titleEl.innerHTML = `Участники <span class="count">(${Number(meta.memberCount || 0) || ''})</span>`;
    if (countEl) countEl.textContent = Number(meta.memberCount || 0) ? `(${Number(meta.memberCount)})` : '';
    if (membersWrap) membersWrap.hidden = false;
    membersPanel.hidden = false;
    membersList.innerHTML = '<div class="dm-profile-panel"><div class="muted">Загрузка участников...</div></div>';

    let participants = [];
    try {
        participants = await api(`/api/dms/${cid}/participants`);
    } catch (e) {
        console.warn('[DM] group participants load failed', e);
    }

    const rows = Array.isArray(participants) ? participants : [];
    if (titleEl) titleEl.innerHTML = `Участники <span class="count">(${rows.length || Number(meta.memberCount || 0) || 0})</span>`;

    const listHtml = rows.map((p) => {
        const id = Number(p?.id || 0);
        const name = (p?.username || 'Unknown').toString();
        const statusClass = statusToClass(p?.is_online === false ? 'offline' : (p?.status || 'offline'));
        return `
          <button class="dm-group-member" type="button" data-user-id="${id}" data-username="${escapeHtml(name)}">
            <span class="avatar small">${avatarInnerHtml(p?.avatar_file_id, name)}</span>
            <span class="dm-group-member-main">
              <span class="dm-group-member-name">${escapeHtml(name)}${p?.is_me ? ' <em>Вы</em>' : ''}</span>
              <span class="dm-group-member-status status-${escapeHtml(statusClass)}">${escapeHtml(statusToLabel(statusClass))}</span>
            </span>
          </button>
        `;
    }).join('');

    membersList.innerHTML = `
      <div class="dm-profile-panel dm-group-profile-panel">
        <div class="dm-group-profile-head">
          <div class="dm-group-profile-avatar">👥</div>
          <div class="dm-profile-meta">
            <div class="dm-profile-name">${escapeHtml(title)}</div>
            <div class="dm-profile-username">${rows.length || Number(meta.memberCount || 0) || 0} участников</div>
          </div>
        </div>
        <div class="dm-group-members-list">
          ${listHtml || '<div class="profile-text empty">Участники не загружены</div>'}
        </div>
      </div>
    `;

    membersList.querySelector('.dm-group-profile-head')?.remove();
    wireAvatarFallbacks(membersList);
    membersList.querySelectorAll('.dm-group-member').forEach((row) => {
        const uid = Number(row.dataset.userId || 0);
        const username = (row.dataset.username || 'Unknown').toString();
        if (!Number.isFinite(uid) || uid <= 0 || uid === Number(currentUser?.id)) return;
        row.addEventListener('click', () => {
            window.dispatchEvent(new CustomEvent('laberry:profile-open', {
                detail: { userId: uid, username },
            }));
        });
    });
    applyDmProfilePanelVisibility();
}

async function renderDmProfile(chatId) {
    const membersPanel = document.getElementById('membersPanel');
    const membersWrap = document.getElementById('membersPanelMembers');
    const membersList = document.getElementById('membersList');
    const countEl = membersPanel?.querySelector('.count');
    const titleEl = membersPanel?.querySelector('.panelHeader h3');
    if (!membersPanel || !membersList) return;

    const cid = Number(chatId);
    if (!isKnownDmChatId(cid)) {
        clearMembersPanelContent();
        membersPanel.hidden = true;
        return;
    }
    const meta = dmMetaByChatId.get(cid) || hiddenDmMeta.get(cid) || {};
    const otherName = (meta.otherName || chatNameById.get(cid) || 'Собеседник').toString();
    const otherId = Number(meta.otherId || 0);

    if (meta.isGroup) {
        await renderGroupDmPanel(cid, meta);
        return;
    }

    if (titleEl) titleEl.innerHTML = 'Профиль';
    if (countEl) countEl.textContent = '';
    if (membersWrap) membersWrap.hidden = false;
    membersPanel.hidden = false;

    membersList.innerHTML = '<div class="dm-profile-panel"><div class="muted">Загрузка профиля...</div></div>';

    let profile = null;
    if (Number.isFinite(otherId) && otherId > 0) {
        try {
            profile = await api(`/api/users/${otherId}/profile`);
        } catch (e) {
            console.warn('[DM] profile load failed', e);
        }
    }

    const display = (profile?.display_name || profile?.username || otherName).toString();
    const username = (profile?.username || otherName).toString();
    const statusRaw = profile?.is_online === false ? 'offline' : (profile?.status || 'online');
    const normalizedStatus = statusToClass(statusRaw);
    const statusClass = normalizedStatus === 'invisible' ? 'offline' : normalizedStatus;
    const statusText = (profile?.status_text || '').toString().trim();
    const about = (profile?.about || '').toString().trim();
    const joinedAt = formatProfileDate(profile?.created_at);
    const joinedText = joinedAt === 'Дата регистрации неизвестна' ? joinedAt : `С нами с ${joinedAt}`;

    membersList.innerHTML = `
      <div class="dm-profile-panel">
        <div class="dm-profile-head">
          <button class="profile-avatar dm-profile-avatar dm-profile-avatar-btn" type="button" title="Открыть профиль">${avatarInnerHtml(profile?.avatar_file_id, display)}</button>
          <div class="dm-profile-meta">
            <div class="dm-profile-name">${escapeHtml(display)}</div>
            <div class="dm-profile-username">@${escapeHtml(username)}</div>
            <div class="profile-presence status-${escapeHtml(statusClass)}">${escapeHtml(statusToLabel(statusClass))}</div>
            <div class="profile-joined">${escapeHtml(joinedText)}</div>
          </div>
        </div>

        <div class="profile-card">
          <div class="profile-label">Статус</div>
          <div class="profile-text${statusText ? '' : ' empty'}">${statusText ? escapeHtml(statusText) : 'Пусто'}</div>
        </div>

        <div class="profile-card">
          <div class="profile-label">О себе</div>
          <div class="profile-text${about ? '' : ' empty'}">${about ? escapeHtml(about) : 'Пользователь ничего не рассказал о себе'}</div>
        </div>

        <div class="profile-card">
          <div class="profile-label">Интеграции</div>
          ${dmProfileConnectionsHtml(profile?.connections)}
        </div>
      </div>
    `;

    wireAvatarFallbacks(membersList);
    const avatarBtn = membersList.querySelector('.dm-profile-avatar-btn');
    if (avatarBtn && Number.isFinite(otherId) && otherId > 0) {
        avatarBtn.addEventListener('click', () => {
            window.dispatchEvent(new CustomEvent('laberry:profile-open', {
                detail: { userId: otherId, username },
            }));
        });
    }
    applyDmProfilePanelVisibility();
}

async function openDmChat(chatId, otherName) {
    currentServerId = null;
    updateServerSelection(null);
    closeAnyServerMenu();

    // If user is in Friends view, chat UI is hidden. Ensure we exit Friends view.
    if (typeof window.closeFriends === 'function') {
        try { window.closeFriends(); } catch (e) { console.warn('[UI] closeFriends failed', e); }
    }

    setUiModeDm();
    setDmHomeActive('');
    await loadDmList(); // refresh active state + order
    await openChat(chatId, otherName);
    renderDmProfile(chatId).catch((e) => console.warn('[DM] render profile failed', e));
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
        const utilityView = document.getElementById('utilityView');
        if (chatView) chatView.hidden = false;
        if (friendsView) friendsView.hidden = true;
        if (utilityView) utilityView.hidden = true;
    } catch (_) {}
    appLog(`[UI] Opening chat ${chatId} (${title})`);
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
        const dmMeta = isDm ? (dmMetaByChatId.get(Number(chatId)) || {}) : {};
        chatTitleElement.textContent = isDm ? (dmMeta.isGroup ? `${title}` : `@ ${title}`) : `# ${title}`;
    }

    const dmCallBtn = document.getElementById('dmCallBtn');
    if (dmCallBtn) {
        const isDm = !currentServerId;
        const dmMeta = isDm ? (dmMetaByChatId.get(Number(chatId)) || {}) : {};
        dmCallBtn.hidden = !isDm || !!dmMeta.isGroup;
    }
    syncDmProfilePanelButtons();
    syncMobileMembersButton();
    refreshDmCallFloat();
    
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

        const rawMsgs = await api(msgsUrl);
        const msgs = Array.isArray(rawMsgs)
            ? await Promise.all(rawMsgs.map((m) => prepareMessageForDisplay(m)))
            : rawMsgs;

        if (seq !== openChatSeq) {
            appLog(`[UI] openChat(${chatId}) stale response ignored (seq=${seq}, current=${openChatSeq})`);
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

    const rawMsgs = await api(url);
    const msgs = Array.isArray(rawMsgs)
        ? await Promise.all(rawMsgs.map((m) => prepareMessageForDisplay(m)))
        : rawMsgs;
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
    window.onChatMessage = async (data) => {
        appLog('[APP] WebSocket message received:', data);
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

        const messageData = await prepareMessageForDisplay(data);
        const sender = (messageData.sender_username || messageData.sender_id || 'Unknown').toString();
        const content = (messageData.content || '').toString();
        const msgId = parseMaybeNumber(messageData.id) || 0;
        const replyToId = parseMaybeNumber(messageData.reply_to_id);
        const replyPreview = (messageData.reply_preview && typeof messageData.reply_preview === 'object') ? messageData.reply_preview : null;
        const senderAvatar = parseMaybeNumber(messageData.sender_avatar_file_id);
        const reactions = Array.isArray(messageData.reactions) ? messageData.reactions : null;

        // ignore echo from yourself
        const myName = (currentUser?.username || currentUser?.nickname || '').toString();
        if (myName && sender === myName) {
            // still append if it's current chat and missing
            if (roomId === currentChatId) {
                addMessage({
                    id: messageData.id,
                    chat_id: roomId,
                    sender_id: messageData.sender_id,
                    sender_username: sender,
                    sender_avatar_file_id: senderAvatar,
                    content,
                    timestamp: messageData.timestamp,
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
                id: messageData.id,
                chat_id: roomId,
                sender_id: messageData.sender_id,
                sender_username: sender,
                sender_avatar_file_id: senderAvatar,
                content,
                timestamp: messageData.timestamp,
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
            appLog('[APP] Attempting WebSocket connection...');
            wsManager.connect(token).then(() => {
                appLog('[APP] WebSocket connected successfully');

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
                    appLog(`[WS] Rejoining room ${currentChatId} after reconnect`);
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
    const composerEmojiBtn = document.getElementById('composerEmojiBtn');
    const fileInput = document.getElementById('fileInput');
    const mdFileInput = document.getElementById('mdFileInput');
    const gifFileInput = document.getElementById('gifFileInput');
    const attachmentsEl = document.getElementById('attachments');
    const sendBtn = document.getElementById('sendBtn');

    const resizeComposerInput = () => {
        if (!input || input.tagName !== 'TEXTAREA') return;
        input.style.height = 'auto';
        input.style.height = `${Math.min(input.scrollHeight, 180)}px`;
    };
    resizeComposerInput();

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

    const normalizeComposerFile = (file, fallbackName = '') => {
        if (!file) return null;

        let f = file;
        let name = (f.name || fallbackName || '').toString().trim();

        // some clipboard images have empty name
        if (!name) {
            const ext = (f.type && f.type.includes('/')) ? f.type.split('/')[1] : 'png';
            name = `pasted_${Date.now()}_${Math.floor(Math.random() * 1000)}.${ext}`;
        }

        const lowerName = name.toLowerCase();
        const rawType = (f.type || '').toString().toLowerCase().split(';')[0].trim();

        // Generated/uploaded .md is intentionally sent as application/octet-stream.
        // The .md filename is the source of truth; server will detect Markdown by extension.
        // This avoids multipart/MIME edge-cases with text/markdown;charset=utf-8 and text/plain.
        const shouldNormalizeMarkdown = lowerName.endsWith('.md') || lowerName.endsWith('.markdown');
        const shouldNormalizeGif = lowerName.endsWith('.gif') || rawType === 'image/gif';
        const safeType = shouldNormalizeMarkdown
            ? 'application/octet-stream'
            : (shouldNormalizeGif ? 'image/gif' : (rawType || 'application/octet-stream'));

        try {
            if (f.name !== name || (f.type || '').toString().toLowerCase() !== safeType) {
                f = new File([f], name, { type: safeType, lastModified: f.lastModified || Date.now() });
            }
        } catch (_) {
            // Some old browsers may not support File constructor for Blob-like objects.
            try { f.name = name; } catch (_) {}
        }

        return { file: f, name, mime: shouldNormalizeMarkdown ? 'text/markdown' : safeType, size: f.size || 0 };
    };

    const addFiles = (files) => {
        if (!files) return;

        const list = Array.from(files);
        let added = 0;

        for (const src of list) {
            const normalized = normalizeComposerFile(src);
            if (!normalized) continue;

            const { file: f, name, mime, size } = normalized;

            // 50MB server limit
            if ((size || 0) > 50 * 1024 * 1024) {
                alert(`Файл слишком большой (лимит 50MB): ${name}`);
                continue;
            }

            pending.push({
                key: `f_${++pendingSeq}`,
                file: f,
                name,
                mime,
                size,
            });
            added++;
        }

        if (added) renderPending();
    };

    const addMarkdownTextAsPendingFile = (text, name) => {
        const body = (text ?? '').toString();
        if (!body.trim()) {
            showToast('Markdown пустой');
            return false;
        }
        const f = makeMarkdownFileFromText(body, name);
        addFiles([f]);
        return true;
    };

    const ensureMarkdownFileModal = () => {
        let overlay = document.getElementById('mdFileModal');
        if (overlay) return overlay;

        overlay = document.createElement('div');
        overlay.className = 'md-file-modal hidden';
        overlay.id = 'mdFileModal';
        overlay.innerHTML = `
          <div class="md-file-dialog" role="dialog" aria-modal="true" aria-label="Markdown файл">
            <div class="md-file-head">
              <div>
                <div class="md-file-title">Markdown-файл</div>
                <div class="md-file-sub">Сделай текст сам или загрузи готовый .md</div>
              </div>
              <button class="md-help-btn" type="button" data-act="help" title="Подсказка Markdown">?</button>
              <button class="md-file-x" type="button" data-act="close">✕</button>
            </div>
            <div class="md-file-body">
              <div class="md-help-panel" hidden>
                <div><b>**жирный**</b> — жирный текст</div>
                <div><b>*курсив*</b> — курсив</div>
                <div><b>&#96;код&#96;</b> — inline-код</div>
                <div><b>&#96;&#96;&#96;js</b> — блок кода</div>
                <div><b># Заголовок</b> — заголовок</div>
                <div><b>&gt; цитата</b> — цитата</div>
                <div><b>- пункт</b> или <b>1. пункт</b> — списки</div>
              </div>
              <label class="md-file-label">Имя файла</label>
              <input class="md-file-name" type="text" maxlength="80" placeholder="message.md" value="message.md">
              <label class="md-file-label">Содержимое</label>
              <textarea class="md-file-text" spellcheck="false" placeholder="# Заголовок\n\nТекст, код, списки..."></textarea>
            </div>
            <div class="md-file-actions">
              <button class="btn btn-ghost" type="button" data-act="upload">Загрузить готовый .md</button>
              <button class="btn btn-primary" type="button" data-act="add">Добавить как файл</button>
            </div>
          </div>
        `;
        document.body.appendChild(overlay);

        const close = () => overlay.classList.add('hidden');
        overlay.addEventListener('click', (ev) => {
            if (ev.target === overlay) close();
            const btn = ev.target?.closest?.('[data-act]');
            if (!btn) return;
            const act = btn.getAttribute('data-act');
            if (act === 'close') {
                close();
                return;
            }
            if (act === 'help') {
                const panel = overlay.querySelector('.md-help-panel');
                if (panel) panel.hidden = !panel.hidden;
                return;
            }
            if (act === 'upload') {
                close();
                mdFileInput?.click?.();
                return;
            }
            if (act === 'add') {
                const nameEl = overlay.querySelector('.md-file-name');
                const textEl = overlay.querySelector('.md-file-text');
                const ok = addMarkdownTextAsPendingFile(textEl?.value || '', nameEl?.value || 'message.md');
                if (ok) {
                    if (textEl) textEl.value = '';
                    close();
                }
            }
        });
        document.addEventListener('keydown', (ev) => {
            if (ev.key === 'Escape' && !overlay.classList.contains('hidden')) close();
        });

        return overlay;
    };

    let attachMenuEl = null;
    let attachMenuBackdrop = null;

    const hideAttachMenu = () => {
        if (attachMenuBackdrop) attachMenuBackdrop.hidden = true;
        if (attachMenuEl) attachMenuEl.hidden = true;
    };

    const ensureAttachMenu = () => {
        if (attachMenuEl && attachMenuBackdrop) return;

        attachMenuBackdrop = document.createElement('div');
        attachMenuBackdrop.className = 'composer-menu-backdrop';
        attachMenuBackdrop.hidden = true;

        attachMenuEl = document.createElement('div');
        attachMenuEl.className = 'composer-attach-menu';
        attachMenuEl.hidden = true;
        attachMenuEl.innerHTML = `
          <button type="button" data-attach-act="files">
            <span class="composer-menu-ic">📎</span>
            <span><b>Файлы</b><small>Изображения, видео, аудио, архивы и любые документы</small></span>
          </button>
          <button type="button" data-attach-act="markdown">
            <span class="composer-menu-ic">MD</span>
            <span><b>Markdown</b><small>Отдельный вид искусства: редактор, файл и предпросмотр в чате</small></span>
          </button>
        `;

        attachMenuEl.addEventListener('click', (ev) => {
            const btn = ev.target?.closest?.('[data-attach-act]');
            if (!btn) return;
            const act = btn.getAttribute('data-attach-act');
            hideAttachMenu();
            if (act === 'files') fileInput?.click?.();
            if (act === 'markdown') {
                const overlay = ensureMarkdownFileModal();
                overlay.classList.remove('hidden');
                overlay.querySelector('.md-file-text')?.focus?.();
            }
        });

        attachMenuBackdrop.addEventListener('click', hideAttachMenu);
        document.addEventListener('keydown', (ev) => {
            if (ev.key === 'Escape') hideAttachMenu();
        });

        document.body.appendChild(attachMenuBackdrop);
        document.body.appendChild(attachMenuEl);
    };

    const openAttachMenu = () => {
        ensureAttachMenu();
        if (!attachMenuEl || !attachMenuBackdrop) return;
        const rect = attachBtn?.getBoundingClientRect?.();
        const x = rect ? rect.left : 18;
        const y = rect ? rect.top - 8 : window.innerHeight - 80;

        attachMenuEl.style.left = '0px';
        attachMenuEl.style.top = '0px';
        attachMenuEl.hidden = false;
        const pad = 10;
        const w = attachMenuEl.offsetWidth || 300;
        const h = attachMenuEl.offsetHeight || 160;
        attachMenuEl.style.left = `${Math.max(pad, Math.min(x, window.innerWidth - w - pad))}px`;
        attachMenuEl.style.top = `${Math.max(pad, Math.min(y - h, window.innerHeight - h - pad))}px`;
        attachMenuBackdrop.hidden = false;
    };

    attachBtn?.addEventListener('click', () => {
        openAttachMenu();
    });

    if (composerEmojiBtn) {
        composerEmojiBtn.hidden = isTouchUi();
        composerEmojiBtn.addEventListener('click', () => {
            openComposerStickerPicker('gifs');
        });
    }

    fileInput?.addEventListener('change', () => {
        addFiles(fileInput.files);
        fileInput.value = '';
    });

    mdFileInput?.addEventListener('change', () => {
        const picked = Array.from(mdFileInput.files || []).filter(f => {
            const n = (f?.name || '').toString().toLowerCase();
            const m = (f?.type || '').toString().toLowerCase();
            return n.endsWith('.md') || n.endsWith('.markdown') || m === 'text/markdown' || m === 'text/plain';
        });
        if (!picked.length && mdFileInput.files?.length) {
            showToast('Выбери .md/.markdown файл');
        }
        addFiles(picked);
        mdFileInput.value = '';
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
        let outgoingContent = c;
        try {
            outgoingContent = await e2eeEncryptForCurrentChat(c);
        } catch (e) {
            console.warn('[E2EE] outgoing encryption failed', e);
            const who = e?.username ? ` (${e.username})` : '';
            const text = e?.code === 'e2ee_public_key_changed'
                ? `Сообщение не отправлено: изменился ключ E2EE${who}. Проверьте собеседника.`
                : 'Сообщение не отправлено: E2EE не готово для всех участников.';
            showToast(text);
            return null;
        }
        const payload = await api(url, {
            method: 'POST',
            body: JSON.stringify({ content: outgoingContent, reply_to_id: replyId || null })
        });

        const msg = {
            id: payload?.id,
            chat_id: currentChatId,
            sender_id: currentUser?.id,
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
    const UPLOAD_PARALLEL_LIMIT = 3;

    const ensureUploadQueueStyles = () => {
        if (document.getElementById('lbUploadQueueStyle')) return;
        const st = document.createElement('style');
        st.id = 'lbUploadQueueStyle';
        st.textContent = `
.upload-queue{padding:10px 12px;border-top:1px solid rgba(255,255,255,.07);border-bottom:1px solid rgba(255,255,255,.07);background:rgba(0,0,0,.14)}
.upload-job{display:grid;grid-template-columns:minmax(0,1fr) auto;gap:10px;align-items:start;padding:10px 0}
.upload-job+.upload-job{border-top:1px solid rgba(255,255,255,.06)}
.upload-job .u-head{display:flex;align-items:center;justify-content:space-between;gap:10px;margin-bottom:4px}
.upload-job .u-name{font-size:13px;font-weight:800;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.upload-job .u-pct{font-size:12px;font-weight:900;color:var(--accent,#a855f7);white-space:nowrap}
.upload-job .u-sub{font-size:12px;opacity:.78;line-height:1.35}
.upload-job .u-bar{height:7px;border-radius:999px;background:rgba(255,255,255,.12);overflow:hidden;margin-top:7px}
.upload-job .u-bar>i{display:block;height:100%;width:0%;background:linear-gradient(90deg,var(--accent,#8b5cf6),#d946ef);transition:width .16s ease}
.upload-job .u-files{display:flex;flex-direction:column;gap:5px;margin-top:8px}
.upload-job .u-file{display:grid;grid-template-columns:minmax(0,1fr) 72px;gap:8px;align-items:center;font-size:11px;opacity:.82}
.upload-job .u-file-name{white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.upload-job .u-file-bar{height:4px;border-radius:999px;background:rgba(255,255,255,.10);overflow:hidden;margin-top:3px}
.upload-job .u-file-bar>i{display:block;height:100%;background:rgba(168,85,247,.9)}
.upload-job .u-file-state{text-align:right;white-space:nowrap;color:#b9c7ff}
.upload-job .u-actions{display:flex;align-items:center;gap:6px}
.upload-job .u-btn{border:1px solid rgba(255,255,255,.18);background:rgba(255,255,255,.06);color:var(--text,#fff);border-radius:10px;padding:7px 10px;cursor:pointer;font-size:12px;font-weight:700}
.upload-job .u-btn:hover{background:rgba(255,255,255,.10)}
`;
        document.head.appendChild(st);
    };
    ensureUploadQueueStyles();

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
            const files = Array.isArray(j.files) ? j.files : [];
            const doneCount = files.filter(f => f && f.fileId).length;
            const activeSet = new Set(Array.isArray(j.activeIndexes) ? j.activeIndexes : []);
            const activeCount = activeSet.size;
            const name = files.length === 1
                ? (files[0]?.name || files[0]?.file?.name || 'file')
                : `${files.length} файлов`;

            let sub = '';
            if (j.status === 'uploading') {
                sub = `${doneCount}/${files.length} файлов • ${formatBytes(loaded)} / ${formatBytes(total)} • потоков: ${activeCount || Math.min(UPLOAD_PARALLEL_LIMIT, files.length)}`;
            } else if (j.status === 'sending') {
                sub = 'Файлы загружены, отправка сообщения…';
            } else if (j.status === 'failed') {
                sub = j.err || 'Ошибка';
            } else if (j.status === 'canceled') {
                sub = 'Отменено';
            }

            const progress = Array.isArray(j.fileProgress) ? j.fileProgress : [];
            const shownFiles = files.slice(0, 5).map((it, idx) => {
                const size = Number(it?.size || it?.file?.size || 0);
                const loadedOne = Math.max(0, Math.min(size || Number(progress[idx] || 0), Number(progress[idx] || 0)));
                const fpct = size > 0 ? Math.max(0, Math.min(100, Math.round((loadedOne / size) * 100))) : (it?.fileId ? 100 : 0);
                const state = it?.fileId ? 'готово' : (activeSet.has(idx) ? `${fpct}%` : (j.status === 'failed' ? '—' : 'очередь'));
                return `
                  <div class="u-file">
                    <div class="u-file-main">
                      <div class="u-file-name">${escapeHtml(it?.name || it?.file?.name || 'file')}</div>
                      <div class="u-file-bar"><i style="width:${fpct}%"></i></div>
                    </div>
                    <div class="u-file-state">${escapeHtml(state)}</div>
                  </div>
                `;
            }).join('');
            const more = files.length > 5
                ? `<div class="u-file"><div class="u-file-name">Ещё ${files.length - 5} файлов в очереди</div><div class="u-file-state"></div></div>`
                : '';

            return `
              <div class="upload-job" data-job="${j.jobId}">
                <div class="u-left">
                  <div class="u-head">
                    <div class="u-name">${escapeHtml(name)}</div>
                    <div class="u-pct">${pct}%</div>
                  </div>
                  <div class="u-sub">${escapeHtml(sub)}</div>
                  <div class="u-bar"><i style="width:${pct}%"></i></div>
                  <div class="u-files">${shownFiles}${more}</div>
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

    const describeUploadError = (err) => {
        const status = Number(err?.status || 0);
        const detail = (err?.detail || '').toString();

        if (detail === 'file_too_large' || status === 413) return 'Файл слишком большой';
        if (detail === 'storage_create_failed') return 'Сервер не может создать storage/files. Проверь права на папку storage или запуск сервера';
        if (detail === 'temp_file_create_failed') return 'Сервер не может создать временный файл в storage/files';
        if (detail === 'temp_file_write_failed' || detail === 'temp_file_flush_failed') return 'Сервер не может записать файл в storage/files';
        if (detail === 'storage_rename_failed') return 'Сервер не может переместить файл в хранилище';
        if (detail === 'db_insert_failed') return 'Файл не был сохранён: ошибка записи в БД';
        if (detail === 'unauthorized' || status === 401) return 'Сессия устарела';
        if (detail === 'forbidden' || detail === 'no_chat_access' || status === 403) return 'Нет доступа загрузить файл в этот чат';
        if (detail === 'unsupported_file_type' || detail === 'bad_mime' || status === 415) return 'Тип файла не принят сервером';
        if (detail === 'missing_file' || detail === 'bad_request' || status === 400) return 'Файл не принят сервером';
        if (detail) return detail;
        if (status) return `Не удалось загрузить файл (${status})`;
        if (String(err?.message || '').includes('upload_network_error')) return 'Ошибка сети при загрузке';
        if (String(err?.message || '').includes('upload_aborted')) return 'Загрузка отменена';
        return 'Не удалось загрузить файл';
    };

    const uploadFileXHR = (file, chatId, onProgress, attachXhr) => {
        return new Promise((resolve, reject) => {
            const xhr = new XMLHttpRequest();
            xhr.open('POST', '/api/files', true);
            xhr.responseType = 'text';

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
                const raw = (() => {
                    try { return xhr.responseText || xhr.response || ''; } catch (_) { return ''; }
                })();
                const parsed = (() => {
                    try { return raw ? JSON.parse(raw) : null; } catch (_) { return null; }
                })();

                const ok = xhr.status >= 200 && xhr.status < 300;
                if (!ok) {
                    const err = new Error(`upload_failed:${xhr.status}`);
                    err.status = xhr.status;
                    err.detail = parsed?.detail || parsed?.error || raw || '';
                    err.response = parsed || raw;
                    reject(err);
                    return;
                }

                resolve(parsed || {});
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

    // ===== GIF library (global + personal favorites) =====
    let gifPickerEl = null;
    let gifPickerBackdrop = null;
    let gifUploadIntent = 'send';
    let stickerScope = 'favorites';
    let gifLibraryCache = { favorites: [], global: [] };
    let gifSearchQuery = '';

    const composerEmojis = [
        '😀','😁','😂','🤣','😊','😍','😘','😎','🤔','😴','😭','😡','👍','👎','🙏','👏','🔥','💯','🎉','❤️','💔','✅','❌','⭐','⚡','🍀','🎧','🎮','📌','📎'
    ];

    const buildFileMarker = (fileId, name, mime = 'image/gif', size = 0) => {
        const cleanMime = String(mime || 'image/gif').toLowerCase().replaceAll('|', '/');
        const cleanName = (name || `gif_${fileId}.gif`).toString();
        return `[[file:${fileId}|${encodeURIComponent(cleanName)}|${cleanMime}|${Number(size || 0)}]]`;
    };

    const ensureGifChat = () => {
        if (currentChatId) return true;
        showToast('Сначала выберите чат');
        return false;
    };

    const renderGifCards = (items, kind) => {
        const query = (gifSearchQuery || '').trim().toLowerCase();
        const source = Array.isArray(items) ? items : [];
        const list = query
            ? source.filter((item) => (item?.original_name || 'animation.gif').toString().toLowerCase().includes(query))
            : source;
        if (!list.length) {
            return `<div class="gif-empty">${kind === 'favorites' ? 'В избранном пока нет стикеров' : 'Глобальные стикеры ещё не добавлены'}</div>`;
        }

        return list.map((item) => {
            const id = Number(item?.id || 0);
            const name = (item?.original_name || 'animation.gif').toString();
            const rawUrl = item?.raw_url || '';
            const favAction = kind === 'favorites'
                ? `<button type="button" class="gif-mini-btn danger" data-gif-act="remove-favorite" data-asset-id="${id}" title="Убрать из избранного">★</button>`
                : (item?.is_favorite
                    ? `<button type="button" class="gif-mini-btn" disabled title="Уже в избранном">★</button>`
                    : `<button type="button" class="gif-mini-btn" data-gif-act="favorite-asset" data-asset-id="${id}" title="Добавить в избранное">☆</button>`);

            return `
              <div class="gif-card" data-asset-id="${id}">
                <button type="button" class="gif-thumb" data-gif-act="send-asset" data-asset-id="${id}" title="Отправить GIF">
                  <img src="${escapeHtml(rawUrl)}" alt="${escapeHtml(name)}" loading="eager" decoding="sync">
                </button>
                <div class="gif-card-bar">
                  <span title="${escapeHtml(name)}">${escapeHtml(name)}</span>
                  ${favAction}
                </div>
              </div>
            `;
        }).join('');
    };

    const renderGifLibrary = (data) => {
        if (!gifPickerEl) return;
        gifLibraryCache = {
            favorites: Array.isArray(data?.favorites) ? data.favorites : [],
            global: Array.isArray(data?.global) ? data.global : [],
        };
        gifPickerEl.querySelectorAll('[data-gif-list="favorites"]').forEach((fav) => {
            fav.innerHTML = renderGifCards(gifLibraryCache.favorites, 'favorites');
        });
        gifPickerEl.querySelectorAll('[data-gif-list="global"]').forEach((global) => {
            global.innerHTML = renderGifCards(gifLibraryCache.global, 'global');
        });
        gifPickerEl.querySelectorAll('[data-sticker-scope]').forEach((btn) => {
            btn.classList.toggle('active', btn.dataset.stickerScope === stickerScope);
        });
        gifPickerEl.querySelectorAll('[data-gif-list]').forEach((el) => {
            el.hidden = el.getAttribute('data-gif-list') !== stickerScope;
        });
    };

    const loadGifLibrary = async () => {
        if (!gifPickerEl) return;
        const fav = gifPickerEl.querySelector('[data-gif-list="favorites"]');
        const global = gifPickerEl.querySelector('[data-gif-list="global"]');
        if (fav) fav.innerHTML = '<div class="gif-empty">Загрузка...</div>';
        if (global) global.innerHTML = '<div class="gif-empty">Загрузка...</div>';
        try {
            const data = await api('/api/gifs');
            renderGifLibrary(data || {});
        } catch (err) {
            console.warn('[GIF] load failed', err);
            if (fav) fav.innerHTML = '<div class="gif-empty">Не удалось загрузить GIF</div>';
            if (global) global.innerHTML = '';
        }
    };

    const hideGifPicker = () => {
        if (gifPickerBackdrop) gifPickerBackdrop.hidden = true;
        if (gifPickerEl) gifPickerEl.hidden = true;
    };

    const sendGifAsset = async (assetId) => {
        const id = Number(assetId);
        if (!Number.isFinite(id) || id <= 0 || !ensureGifChat()) return;
        try {
            const res = await api('/api/gifs/clone', {
                method: 'POST',
                body: JSON.stringify({ asset_id: id, chat_id: Number(currentChatId) })
            });
            const fileId = res?.file_id ?? res?.fileId ?? res?.id;
            if (!fileId) throw new Error('gif_clone_no_file_id');
            await sendMessage(buildFileMarker(fileId, res?.original_name || 'animation.gif', res?.mime_type || 'image/gif', res?.file_size || 0));
            hideGifPicker();
        } catch (err) {
            console.warn('[GIF] send failed', err);
            showToast('Не удалось отправить GIF');
        }
    };

    const favoriteGifAsset = async (assetId) => {
        const id = Number(assetId);
        if (!Number.isFinite(id) || id <= 0) return;
        try {
            await api('/api/gifs/favorites', {
                method: 'POST',
                body: JSON.stringify({ asset_id: id })
            });
            showToast('GIF добавлен в избранное');
            await loadGifLibrary();
        } catch (err) {
            console.warn('[GIF] favorite failed', err);
            showToast('Не удалось добавить GIF');
        }
    };

    const removeFavoriteGif = async (assetId) => {
        const id = Number(assetId);
        if (!Number.isFinite(id) || id <= 0) return;
        try {
            await api(`/api/gifs/favorites/${id}`, { method: 'DELETE' });
            showToast('GIF убран из избранного');
            await loadGifLibrary();
        } catch (err) {
            console.warn('[GIF] remove favorite failed', err);
            showToast('Не удалось убрать GIF');
        }
    };

    const ensureGifPicker = () => {
        if (gifPickerEl && gifPickerBackdrop) return;

        gifPickerBackdrop = document.createElement('div');
        gifPickerBackdrop.className = 'gif-backdrop';
        gifPickerBackdrop.hidden = true;

        gifPickerEl = document.createElement('div');
        gifPickerEl.className = 'gif-picker';
        gifPickerEl.hidden = true;
        gifPickerEl.innerHTML = `
          <div class="gif-picker-head">
            <div>
              <div class="gif-picker-title">Эмодзи и стикеры</div>
              <div class="gif-picker-sub">Эмодзи для текста и анимированные стикеры из списка</div>
            </div>
            <button type="button" class="gif-close" data-gif-act="close" title="Закрыть">×</button>
          </div>
          <div class="composer-picker-tabs">
            <button type="button" class="active" data-picker-tab="emoji">Эмодзи</button>
            <button type="button" data-picker-tab="stickers">Стикеры</button>
          </div>
          <section class="composer-picker-panel active" data-picker-panel="emoji">
            <div class="emoji-grid composer-emoji-grid">
              ${composerEmojis.map(e => `<button type="button" class="emoji-btn" data-composer-emoji="${escapeHtml(e)}">${escapeHtml(e)}</button>`).join('')}
            </div>
          </section>
          <section class="composer-picker-panel" data-picker-panel="stickers">
            <div class="gif-picker-actions">
              <button type="button" data-gif-act="upload-send">Загрузить и отправить</button>
              <button type="button" data-gif-act="upload-favorite">В избранное</button>
            </div>
            <div class="sticker-scope-tabs">
              <button type="button" class="active" data-sticker-scope="favorites">Избранное</button>
              <button type="button" data-sticker-scope="global">Стикеры</button>
            </div>
            <section class="gif-section">
              <div class="gif-grid" data-gif-list="favorites"></div>
              <div class="gif-grid" data-gif-list="global" hidden></div>
            </section>
          </section>
        `;

        gifPickerEl.className = 'gif-picker discord-picker';
        gifPickerEl.innerHTML = `
          <div class="discord-picker-top">
            <div class="composer-picker-tabs">
              <button type="button" class="active" data-picker-tab="gifs">Гифки</button>
              <button type="button" data-picker-tab="stickers">Стикеры</button>
              <button type="button" data-picker-tab="emoji">Эмодзи</button>
            </div>
            <button type="button" class="gif-close" data-gif-act="close" title="Закрыть">×</button>
          </div>
          <div class="gif-search-row" data-gif-search-wrap>
            <input class="gif-search-input" type="search" data-gif-search placeholder="Поиск GIF">
          </div>
          <section class="composer-picker-panel active" data-picker-panel="gifs">
            <div class="gif-category-grid">
              <button type="button" class="gif-category-card favorite" data-gif-category data-sticker-scope="favorites">
                <span>Избранное</span>
              </button>
              <button type="button" class="gif-category-card popular" data-gif-category data-sticker-scope="global">
                <span>Популярные GIF</span>
              </button>
              <button type="button" class="gif-category-card calm" data-gif-category data-sticker-scope="global">
                <span>Милые</span>
              </button>
              <button type="button" class="gif-category-card mood" data-gif-category data-sticker-scope="global">
                <span>Реакции</span>
              </button>
            </div>
          </section>
          <section class="composer-picker-panel" data-picker-panel="stickers">
            <div class="sticker-scope-tabs">
              <button type="button" class="active" data-sticker-scope="favorites">Избранное</button>
              <button type="button" data-sticker-scope="global">Стикеры</button>
            </div>
            <section class="gif-section">
              <div class="gif-grid" data-gif-list="favorites"></div>
              <div class="gif-grid" data-gif-list="global" hidden></div>
            </section>
          </section>
          <section class="composer-picker-panel" data-picker-panel="emoji">
            <div class="emoji-grid composer-emoji-grid">
              ${composerEmojis.map(e => `<button type="button" class="emoji-btn" data-composer-emoji="${escapeHtml(e)}">${escapeHtml(e)}</button>`).join('')}
            </div>
          </section>
        `;

        gifPickerEl.addEventListener('click', (e) => {
            const emojiBtn = e.target?.closest?.('[data-composer-emoji]');
            if (emojiBtn) {
                insertTextIntoComposer(emojiBtn.getAttribute('data-composer-emoji') || '');
                hideGifPicker();
                return;
            }

            const tabBtn = e.target?.closest?.('[data-picker-tab]');
            if (tabBtn) {
                const tab = tabBtn.dataset.pickerTab || 'gifs';
                gifPickerEl.querySelectorAll('[data-picker-tab]').forEach((x) => x.classList.toggle('active', x === tabBtn));
                gifPickerEl.querySelectorAll('[data-picker-panel]').forEach((panel) => {
                    panel.classList.toggle('active', panel.dataset.pickerPanel === tab);
                });
                const searchWrap = gifPickerEl.querySelector('[data-gif-search-wrap]');
                if (searchWrap) searchWrap.hidden = tab === 'emoji';
                if (tab !== 'emoji') loadGifLibrary();
                return;
            }

            const scopeBtn = e.target?.closest?.('[data-sticker-scope]');
            if (scopeBtn) {
                stickerScope = scopeBtn.dataset.stickerScope || 'favorites';
                gifPickerEl.querySelectorAll('[data-sticker-scope]').forEach((x) => x.classList.toggle('active', x === scopeBtn));
                gifPickerEl.querySelectorAll('[data-gif-list]').forEach((el) => {
                    el.hidden = el.getAttribute('data-gif-list') !== stickerScope;
                });
                if (scopeBtn.hasAttribute('data-gif-category')) {
                    const stickersTab = gifPickerEl.querySelector('[data-picker-tab="stickers"]');
                    if (stickersTab) stickersTab.click();
                    else loadGifLibrary();
                }
                return;
            }

            const btn = e.target?.closest?.('[data-gif-act]');
            if (!btn) return;
            const act = btn.getAttribute('data-gif-act');
            const assetId = btn.getAttribute('data-asset-id');
            if (act === 'close') hideGifPicker();
            if (act === 'send-asset') sendGifAsset(assetId);
            if (act === 'favorite-asset') favoriteGifAsset(assetId);
            if (act === 'remove-favorite') removeFavoriteGif(assetId);
            if (act === 'upload-send' || act === 'upload-favorite') {
                if (!ensureGifChat()) return;
                gifUploadIntent = act === 'upload-send' ? 'send' : 'favorite';
                gifFileInput?.click?.();
            }
        });

        gifPickerEl.querySelector('[data-gif-search]')?.addEventListener('input', (e) => {
            gifSearchQuery = (e.target?.value || '').toString();
            renderGifLibrary(gifLibraryCache);
        });

        gifPickerBackdrop.addEventListener('click', () => hideGifPicker());
        document.addEventListener('keydown', (e) => {
            if (e.key === 'Escape') hideGifPicker();
        });

        document.body.appendChild(gifPickerBackdrop);
        document.body.appendChild(gifPickerEl);
    };

    const openComposerStickerPicker = (initialTab = 'gifs') => {
        if (!ensureGifChat()) return;
        ensureGifPicker();
        if (!gifPickerEl || !gifPickerBackdrop) return;

        let x = window.innerWidth / 2;
        let y = window.innerHeight - 84;
        const anchor = composerEmojiBtn;
        if (anchor?.getBoundingClientRect) {
            const r = anchor.getBoundingClientRect();
            x = r.left;
            y = r.top - 8;
        }

        const tab = ['gifs', 'stickers', 'emoji'].includes(initialTab) ? initialTab : 'gifs';
        gifPickerEl.querySelectorAll('[data-picker-tab]').forEach((btn) => {
            btn.classList.toggle('active', btn.dataset.pickerTab === tab);
        });
        gifPickerEl.querySelectorAll('[data-picker-panel]').forEach((panel) => {
            panel.classList.toggle('active', panel.dataset.pickerPanel === tab);
        });
        const searchWrap = gifPickerEl.querySelector('[data-gif-search-wrap]');
        if (searchWrap) searchWrap.hidden = tab === 'emoji';

        gifPickerEl.style.left = '0px';
        gifPickerEl.style.top = '0px';
        gifPickerEl.hidden = false;
        const pad = 10;
        const w = gifPickerEl.offsetWidth || 380;
        const h = gifPickerEl.offsetHeight || 480;
        gifPickerEl.style.left = `${Math.max(pad, Math.min(x, window.innerWidth - w - pad))}px`;
        gifPickerEl.style.top = `${Math.max(pad, Math.min(y - h, window.innerHeight - h - pad))}px`;
        gifPickerBackdrop.hidden = false;
        if (tab !== 'emoji') loadGifLibrary();
    };

    gifFileInput?.addEventListener('change', async () => {
        const file = gifFileInput.files?.[0] || null;
        gifFileInput.value = '';
        if (!file) return;
        if (!ensureGifChat()) return;

        const normalized = normalizeComposerFile(file, file.name || 'animation.gif');
        const name = normalized?.name || file.name || 'animation.gif';
        const mime = normalized?.mime || file.type || 'image/gif';
        if (!/\.gif$/i.test(name) && !String(mime).toLowerCase().startsWith('image/gif')) {
            showToast('Выберите GIF-файл');
            return;
        }

        try {
            const res = await uploadFileXHR(normalized.file, Number(currentChatId), null, null);
            const fileId = res?.id ?? res?.file_id ?? res?.fileId;
            if (!fileId) throw new Error('gif_upload_no_file_id');
            await api('/api/gifs/favorites', {
                method: 'POST',
                body: JSON.stringify({ file_id: Number(fileId) })
            });
            if (gifUploadIntent === 'send') {
                await sendMessage(buildFileMarker(fileId, name, 'image/gif', normalized.size || file.size || 0));
                hideGifPicker();
            } else {
                showToast('GIF добавлен в избранное');
                await loadGifLibrary();
            }
        } catch (err) {
            console.warn('[GIF] upload failed', err);
            showToast('Не удалось загрузить GIF');
        }
    });

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

    const recalcUploadJobProgress = (job) => {
        const progress = Array.isArray(job.fileProgress) ? job.fileProgress : [];
        job.loadedBytes = progress.reduce((sum, n) => sum + (Number(n || 0) || 0), 0);
        if (job.totalBytes > 0) job.loadedBytes = Math.min(job.loadedBytes, job.totalBytes);
    };

    const startUploadJob = async (job) => {
        job.status = 'uploading';
        job.loadedBytes = 0;
        job.totalBytes = job.files.reduce((a, it) => a + (Number(it.size || it.file?.size || 0) || 0), 0);
        job.xhrs = [];
        job.fileProgress = job.files.map(() => 0);
        job.activeIndexes = [];
        renderUploadQueue();

        wsSendState('upload_state', 'start', job.activity);

        let nextIndex = 0;
        const concurrency = Math.max(1, Math.min(UPLOAD_PARALLEL_LIMIT, job.files.length));
        const takeNextIndex = () => {
            if (job.status === 'canceled') return -1;
            if (nextIndex >= job.files.length) return -1;
            const idx = nextIndex;
            nextIndex += 1;
            return idx;
        };

        const worker = async () => {
            while (true) {
                const idx = takeNextIndex();
                if (idx < 0) return;

                const it = job.files[idx];
                if (!it) continue;
                const f = it.file;
                if (!f) continue;

                job.activeIndexes = Array.from(new Set([...(job.activeIndexes || []), idx]));
                renderUploadQueue();

                try {
                    const res = await uploadFileXHR(f, job.chatId, (loaded, total) => {
                        const curTotal = Number(total || it.size || f.size || 0);
                        const curLoaded = Number(loaded || 0);
                        job.fileProgress[idx] = Math.min(curTotal || curLoaded, curLoaded);
                        recalcUploadJobProgress(job);
                        renderUploadQueue();
                    }, (xhr) => {
                        job.xhrs.push(xhr);
                    });

                    const id = res?.id ?? res?.file_id ?? res?.fileId;
                    if (!id) {
                        const err = new Error('upload_failed:no_file_id');
                        err.detail = 'Сервер не вернул ID файла';
                        throw err;
                    }
                    it.fileId = id;
                    job.fileProgress[idx] = Number(it.size || f.size || 0) || job.fileProgress[idx] || 0;
                    recalcUploadJobProgress(job);
                    renderUploadQueue();
                } finally {
                    job.activeIndexes = (job.activeIndexes || []).filter(v => v !== idx);
                    renderUploadQueue();
                }
            }
        };

        try {
            const workers = Array.from({ length: concurrency }, () => worker());
            await Promise.all(workers);

            if (job.status === 'canceled') throw new Error('canceled');

            const missing = job.files.find(it => !it.fileId);
            if (missing) {
                const err = new Error('upload_failed:missing_file_id');
                err.detail = 'Не все файлы были загружены';
                throw err;
            }

            job.loadedBytes = job.totalBytes;
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

            wsSendState('upload_state', 'stop', job.activity);
            const idx = uploadJobs.findIndex(j => j.jobId === job.jobId);
            if (idx >= 0) uploadJobs.splice(idx, 1);
            renderUploadQueue();
        } catch (e) {
            if (String(e?.message || '').includes('canceled') || String(e?.message || '').includes('aborted')) {
                job.status = 'canceled';
            } else {
                job.status = 'failed';
                job.err = describeUploadError(e);
                console.warn('[UPLOAD] job failed', e);
            }
            try {
                for (const x of (job.xhrs || [])) {
                    try { x.abort(); } catch (_) {}
                }
            } catch (_) {}
            job.activeIndexes = [];
            wsSendState('upload_state', 'stop', job.activity);
            renderUploadQueue();
        }
    };

    form.addEventListener('submit', async (e) => {
        e.preventDefault();
        e.stopImmediatePropagation();

        const rawInputText = (input?.value || '').toString();
        let text = rawInputText.trim();
        const files = pending.slice();

        if (!text && files.length === 0) return;

        if (text.length > MESSAGE_TEXT_FILE_THRESHOLD) {
            const f = makeMarkdownFileFromText(rawInputText, buildMarkdownAttachmentName('message'));
            files.unshift({
                key: `auto_md_${Date.now()}`,
                file: f,
                name: f.name || buildMarkdownAttachmentName('message'),
                mime: 'text/markdown',
                size: Number(f.size || 0),
            });
            text = '';
            showToast('Длинный текст отправится как .md файл');
        }
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
        if (input) {
            input.value = '';
            resizeComposerInput();
        }
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
                if (input && text) {
                    input.value = text;
                    resizeComposerInput();
                }
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
            fileProgress: [],
            activeIndexes: [],
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

    input?.addEventListener('input', () => {
        resizeComposerInput();
        touchTyping();
    });
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

    appLog('[APP] Message composer setup complete');
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


function isGoneError(err) {
    return Number(err?.status) === 410;
}

function markAttachmentExpired(att, text = 'Файл больше недоступен') {
    if (!att || att.dataset.expired === '1') return;

    att.dataset.expired = '1';
    att.classList.add('is-expired');
    att.removeAttribute('data-links-wired');

    const name = (att.getAttribute('data-file-name') || 'Вложение').toString();
    const mime = (att.getAttribute('data-file-mime') || '').toString();
    const size = (att.getAttribute('data-file-size') || '').toString();
    const lowerName = name.toLowerCase();
    const ext = lowerName.includes('.') ? lowerName.split('.').pop() : '';
    const badge = (ext || (mime.split('/')[1] || mime.split('/')[0] || 'file')).toString().slice(0, 6).toUpperCase();
    const meta = [size ? formatBytes(size) : '', 'истёк срок хранения'].filter(Boolean).join(' • ');

    att.innerHTML = `
      <div class="att-expired" title="${escapeHtml(text)}">
        <span class="att-expired-icon" aria-hidden="true">⌛</span>
        <span class="att-expired-main">
          <span class="att-expired-name">${escapeHtml(name)}</span>
          <span class="att-expired-meta">${escapeHtml(meta || text)}</span>
        </span>
        <span class="att-expired-badge">${escapeHtml(badge || 'FILE')}</span>
      </div>
    `;
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

let rawHref = '';
let dlHref = '';
let previewHref = `/api/files/${id}/preview`;
let originalAvailable = true;
try {
    const links = await getFileLinks(id);
    originalAvailable = links?.original_available !== false && links?.download_available !== false;
    rawHref = (links?.raw_url || '').toString();
    dlHref = (links?.download_url || '').toString();
    if (links?.preview_url) previewHref = links.preview_url;
} catch (err) {
    if (isGoneError(err)) {
        if (this.bodyEl) {
            this.bodyEl.innerHTML = `<div class="av-unknown"><div class="av-unknown-name">${escapeHtml(name)}</div><div class="av-unknown-hint">Файл больше недоступен: истёк срок хранения.</div></div>`;
            this.mediaEl = null;
            this.setZoom(1);
            this.setZoomButtons(false);
        }
        if (this.dlEl) {
            this.dlEl.href = '#';
            delete this.dlEl.dataset.fileId;
            delete this.dlEl.dataset.fileName;
        }
        if (this.openEl) this.openEl.href = '#';
        overlay.classList.remove('hidden');
        return;
    }
}

if (this.dlEl) {
    if (dlHref && originalAvailable) {
        this.dlEl.href = dlHref;
        this.dlEl.dataset.fileId = String(id);
        this.dlEl.dataset.fileName = name;
        this.dlEl.classList.remove('is-disabled');
        this.dlEl.removeAttribute('aria-disabled');
        this.dlEl.title = 'Скачать';
    } else {
        this.dlEl.href = '#';
        delete this.dlEl.dataset.fileId;
        delete this.dlEl.dataset.fileName;
        this.dlEl.classList.add('is-disabled');
        this.dlEl.setAttribute('aria-disabled', 'true');
        this.dlEl.title = 'Оригинал файла удалён';
    }
}
if (this.openEl) {
    if (rawHref && originalAvailable) {
        this.openEl.href = rawHref;
        this.openEl.classList.remove('is-disabled');
        this.openEl.removeAttribute('aria-disabled');
        this.openEl.title = 'Открыть в браузере';
    } else {
        this.openEl.href = '#';
        this.openEl.classList.add('is-disabled');
        this.openEl.setAttribute('aria-disabled', 'true');
        this.openEl.title = 'Оригинал файла удалён';
    }
}

            if (this.bodyEl) {
                this.bodyEl.innerHTML = '';
                this.mediaEl = null;
                this.setZoom(1);

                if (mime.startsWith('image/')) {
                    const img = document.createElement('img');
                    img.className = 'av-media av-img';
                    img.alt = name;
                    img.loading = 'lazy';
                    img.decoding = 'async';

                    const previewSrc = previewHref || rawHref;
                    const rawSrc = rawHref || '';
                    let triedRaw = false;
                    if (!originalAvailable && this.metaEl) {
                        const currentMeta = this.metaEl.textContent || '';
                        this.metaEl.textContent = currentMeta ? `${currentMeta} • оригинал удалён` : 'оригинал удалён';
                    }

                    const sameUrl = (a, b) => {
                        try { return new URL(a, location.href).href === new URL(b, location.href).href; }
                        catch (_) { return String(a || '') === String(b || ''); }
                    };

                    const tryRaw = () => {
                        if (triedRaw || !rawSrc || sameUrl(img.src, rawSrc)) return;
                        triedRaw = true;
                        const pre = new Image();
                        pre.onload = () => { img.src = rawSrc; };
                        pre.onerror = () => {};
                        pre.src = rawSrc;
                    };

                    img.src = previewSrc;
                    img.addEventListener('load', () => {
                        if (this.zoom === 1) this.fitViewerToImage(img);
                        tryRaw();
                    });
                    img.addEventListener('error', () => {
                        if (!triedRaw && rawSrc && !sameUrl(img.src, rawSrc)) {
                            triedRaw = true;
                            img.src = rawSrc;
                            return;
                        }
                        if (this.bodyEl) {
                            const hint = originalAvailable
                                ? 'Не удалось открыть изображение в предпросмотре. Попробуй скачать файл.'
                                : 'Оригинал файла удалён. Превью тоже недоступно.';
                            this.bodyEl.innerHTML = '<div class="av-unknown"><div class="av-unknown-name">' + escapeHtml(name) + '</div><div class="av-unknown-hint">' + escapeHtml(hint) + '</div></div>';
                            this.mediaEl = null;
                            this.setZoomButtons(false);
                        }
                    });
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
    overlay.querySelector('#avDownload')?.addEventListener('click', (e) => {
        e.preventDefault();
        e.stopPropagation();
        triggerSignedAttachmentDownload(e.currentTarget);
    });
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
        if (isGoneError(e)) {
            v.bodyEl.innerHTML = `<div class="muted" style="padding:12px;">Файл больше недоступен: истёк срок хранения.</div>`;
        } else {
            v.bodyEl.innerHTML = `<div class="muted" style="padding:12px;">Просмотр содержимого недоступен.</div>`;
        }
    }
}

const textAttachmentPreviewCache = new Map();

async function toggleTextAttachmentPreview(att) {
    if (!att) return;
    const already = att.querySelector('.text-attachment-preview');

    document.querySelectorAll('.text-attachment-preview').forEach(el => {
        if (el !== already) el.remove();
    });
    document.querySelectorAll('.msg-attachment.text-open').forEach(el => {
        if (el !== att) el.classList.remove('text-open');
    });

    if (already) {
        already.remove();
        att.classList.remove('text-open');
        return;
    }

    const id = att.getAttribute('data-file-id');
    const name = att.getAttribute('data-file-name') || 'text.md';
    const mime = att.getAttribute('data-file-mime') || '';
    const box = document.createElement('div');
    box.className = 'text-attachment-preview loading';
    box.innerHTML = `<div class="text-preview-head"><span>${escapeHtml(name)}</span><span>Загрузка…</span></div>`;
    att.appendChild(box);
    att.classList.add('text-open');

    try {
        const links = await getFileLinks(id);
        const rawUrl = links?.raw_url || links?.download_url;
        if (!rawUrl || links?.download_available === false) throw new Error('no_raw_url');

        const res = await fetch(rawUrl, {
            method: 'GET',
            headers: { 'Range': `bytes=0-${TEXT_ATTACHMENT_PREVIEW_BYTES - 1}` },
            credentials: 'same-origin',
        });
        if (!res.ok && res.status !== 206) throw new Error(`text_fetch_${res.status}`);

        let txt = await res.text();
        let clipped = false;
        if (txt.length > TEXT_ATTACHMENT_MAX_RENDER_CHARS) {
            txt = txt.slice(0, TEXT_ATTACHMENT_MAX_RENDER_CHARS);
            clipped = true;
        }
        const contentRange = res.headers.get('content-range') || '';
        if (res.status === 206 || /\/\d+/.test(contentRange)) clipped = true;

        textAttachmentPreviewCache.set(String(id), { name, mime, text: txt, clipped });

        const isMd = isMarkdownAttachmentFile(name, mime);
        const renderedBody = isMd
            ? renderMarkdownText(txt, { full: false })
            : `<pre class="text-preview-body is-plain">${escapeHtml(txt)}</pre>`;

        box.className = 'text-attachment-preview open' + (isMd ? ' markdown-open' : '');
        const headLabel = isMd ? (clipped ? 'Markdown-фрагмент' : 'Markdown') : (clipped ? 'Показан фрагмент' : 'Открыто');
        box.innerHTML = `
          <div class="text-preview-head">
            <span>${escapeHtml(name)}</span>
            <div class="text-preview-actions">
              ${isMd ? '<button type="button" class="text-preview-full">В большое окно</button>' : ''}
              <button type="button" class="text-preview-collapse">Свернуть</button>
              <span>${headLabel}</span>
            </div>
          </div>
          <div class="text-preview-body ${isMd ? 'is-markdown' : 'is-plain-wrap'}">${renderedBody}</div>
        `;
    } catch (err) {
        box.className = 'text-attachment-preview error';
        box.innerHTML = `
          <div class="text-preview-head"><span>${escapeHtml(name)}</span><span>Ошибка</span></div>
          <div class="text-preview-error">Не удалось открыть предпросмотр. Скачивание может работать.</div>
        `;
    }
}

function setupAttachmentUi() {
    if (attachmentUiReady) return;
    const container = $("messages");
    if (!container) return;

    // open viewer (image click / file-row click)
    container.addEventListener('click', (e) => {
        const dl = e.target?.closest?.('.att-dl');
        if (dl) {
            e.preventDefault();
            e.stopPropagation();
            triggerSignedAttachmentDownload(dl);
            return;
        }

        const favBtn = e.target?.closest?.('.att-fav');
        if (favBtn) {
            e.preventDefault();
            e.stopPropagation();
            const att = favBtn.closest('.msg-attachment');
            const fileId = Number(att?.getAttribute?.('data-file-id') || 0);
            if (!Number.isFinite(fileId) || fileId <= 0) return;
            try {
                api('/api/gifs/favorites', {
                    method: 'POST',
                    body: JSON.stringify({ file_id: fileId })
                })
                    .then(() => {
                        favBtn.textContent = '★';
                        favBtn.classList.add('saved');
                        favBtn.setAttribute('title', 'GIF в избранном');
                        showToast('GIF добавлен в избранное');
                    })
                    .catch((err) => {
                        console.warn('[GIF] favorite from attachment failed', err);
                        showToast('Не удалось добавить GIF');
                    });
            } catch (err) {
                console.warn('[GIF] favorite from attachment failed', err);
            }
            return;
        }

        const archBtn = e.target?.closest?.('.att-archive');
        if (archBtn) {
            e.preventDefault();
            e.stopPropagation();
            const att = archBtn.closest('.msg-attachment');
            if (!att) return;
            if (att.dataset.expired === '1') {
                showToast('Файл больше недоступен');
                return;
            }
            openArchiveViewer({
                id: att.getAttribute('data-file-id'),
                name: att.getAttribute('data-file-name')
            });
            return;
        }

        const collapseText = e.target?.closest?.('.text-preview-collapse');
        if (collapseText) {
            e.preventDefault();
            e.stopPropagation();
            const att = collapseText.closest('.msg-attachment');
            att?.querySelector?.('.text-attachment-preview')?.remove?.();
            att?.classList?.remove?.('text-open');
            return;
        }

        const fullText = e.target?.closest?.('.text-preview-full');
        if (fullText) {
            e.preventDefault();
            e.stopPropagation();
            const att = fullText.closest('.msg-attachment');
            const id = att?.getAttribute?.('data-file-id');
            const cached = id ? textAttachmentPreviewCache.get(String(id)) : null;
            if (cached?.text) {
                openFullMarkdownText(cached.name || 'Markdown', cached.text, cached.clipped ? 'Показан загруженный фрагмент файла' : 'Полный загруженный текст');
            } else {
                showToast('Сначала открой предпросмотр файла');
            }
            return;
        }

        const textBtn = e.target?.closest?.('.att-text-toggle');
        if (textBtn) {
            e.preventDefault();
            e.stopPropagation();
            const att = textBtn.closest('.msg-attachment');
            if (!att) return;
            if (att.dataset.expired === '1') {
                showToast('Файл больше недоступен');
                return;
            }
            toggleTextAttachmentPreview(att).catch(() => showToast('Не удалось открыть текстовый файл'));
            return;
        }

        const playBtn = e.target?.closest?.('.att-play');
        if (playBtn) {
            e.preventDefault();
            e.stopPropagation();
            const att = playBtn.closest('.msg-attachment');
            const video = att?.querySelector?.('video.att-video');
            if (!video) return;
            if (video.paused || video.ended) {
                video.play().catch(() => {});
            } else {
                video.pause();
            }
            return;
        }

        const img = e.target?.closest?.('img.att-img');
        if (img) {
            const att = img.closest('.msg-attachment');
            if (!att) return;
            if (att.dataset.expired === '1') {
                showToast('Файл больше недоступен');
                return;
            }
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
            if (att.dataset.expired === '1') {
                showToast('Файл больше недоступен');
                return;
            }
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
        if (att.dataset.expired === '1') {
            showToast('Файл больше недоступен');
            return;
        }
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

function isDisabledFileAction(el) {
    return !!el?.closest?.('.is-disabled,[aria-disabled="true"]');
}

async function triggerSignedAttachmentDownload(linkEl) {
    const el = linkEl;
    if (!el) return;
    if (isDisabledFileAction(el)) {
        showToast('Оригинал файла удалён. Доступно только превью.');
        return;
    }
    if (el.dataset.downloading === '1') return;

    const att = el.closest?.('.msg-attachment');
    const fileId = (el.dataset.fileId || att?.getAttribute?.('data-file-id') || '').toString().trim();
    const fileName = (el.dataset.fileName || att?.getAttribute?.('data-file-name') || '').toString().trim();

    if (!fileId) {
        const fallbackHref = (el.getAttribute('href') || '').trim();
        if (!fallbackHref || fallbackHref === '#') {
            showToast('Не удалось скачать файл');
            return;
        }
        const fallbackLink = document.createElement('a');
        fallbackLink.href = fallbackHref;
        if (fileName) fallbackLink.download = fileName;
        fallbackLink.rel = 'noopener';
        fallbackLink.style.display = 'none';
        document.body.appendChild(fallbackLink);
        fallbackLink.click();
        setTimeout(() => { try { fallbackLink.remove(); } catch (_) {} }, 0);
        return;
    }

    el.dataset.downloading = '1';
    el.classList.add('is-loading');

    try {
        const links = await getFileLinks(fileId);
        const href = (links?.download_url || '').toString().trim();
        if (!href || links?.download_available === false || links?.original_available === false) {
            showToast('Оригинал файла удалён. Доступно только превью.');
            return;
        }

        if (att) {
            att.querySelectorAll?.('a.att-dl')?.forEach?.((a) => {
                a.href = href;
                a.dataset.fileId = fileId;
                if (fileName) a.dataset.fileName = fileName;
            });
        }

        el.href = href;
        el.dataset.fileId = fileId;
        if (fileName) el.dataset.fileName = fileName;

        const a = document.createElement('a');
        a.href = href;
        if (fileName) a.download = fileName;
        a.rel = 'noopener';
        a.style.display = 'none';
        document.body.appendChild(a);
        a.click();
        setTimeout(() => { try { a.remove(); } catch (_) {} }, 0);
    } catch (err) {
        console.warn('[ATTACH] signed download failed', err);
        if (isGoneError(err)) {
            if (att) markAttachmentExpired(att);
            showToast('Файл больше недоступен');
        } else {
            showToast('Не удалось скачать файл');
        }
    } finally {
        delete el.dataset.downloading;
        el.classList.remove('is-loading');
    }
}

function wireAttachments(root) {

if (!root) return;

// initialize videos (wait for signed link, keep state classes in sync)
root.querySelectorAll?.('video.att-video')?.forEach?.((v) => {
    if (v.dataset.wired === '1') return;
    const att = v.closest?.('.msg-attachment');
    const syncState = () => {
        if (!att) return;
        const isPlaying = !!(v.currentSrc && !v.paused && !v.ended);
        att.classList.toggle('is-playing', isPlaying);
        att.classList.toggle('is-ready', !!v.currentSrc);
        const playBtn = att.querySelector?.('.att-play');
        if (playBtn) {
            const title = isPlaying ? 'Пауза' : 'Воспроизвести';
            playBtn.setAttribute('title', title);
            playBtn.setAttribute('aria-label', title);
        }
    };

    v.controls = true;
    v.playsInline = true;
    v.preload = 'metadata';
    ['play', 'pause', 'ended', 'loadedmetadata', 'canplay', 'emptied'].forEach((evt) => {
        v.addEventListener(evt, syncState);
    });
    syncState();
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

        const originalAvailable = links?.original_available !== false && links?.download_available !== false && !!links.download_url;
        att.classList.toggle('is-thumb-only', !originalAvailable && links?.thumb_available === true);

        // download buttons (there can be multiple in one attachment)
        att.querySelectorAll?.('a.att-dl')?.forEach?.((a) => {
            const fileName = att.getAttribute('data-file-name');
            if (originalAvailable) {
                a.dataset.fileId = String(id);
                if (fileName) a.dataset.fileName = fileName;
                if (links.download_url) a.href = links.download_url;
                a.classList.remove('is-disabled');
                a.removeAttribute('aria-disabled');
                a.title = 'Скачать';
            } else {
                a.href = '#';
                delete a.dataset.fileId;
                delete a.dataset.fileName;
                a.classList.add('is-disabled');
                a.setAttribute('aria-disabled', 'true');
                a.title = 'Оригинал файла удалён';
            }
        });

        // image preview + raw
        const img = att.querySelector?.('img.att-img');
        if (img) {
            const rawUrl = (links.raw_url || '').toString();
            const previewUrl = (links.preview_url || rawUrl || '').toString();

            if (rawUrl) img.setAttribute('data-raw-src', rawUrl);

            const holder = img.closest?.('.att-preview-image');
            const setOverlayLabel = (className, text) => {
                if (!holder) return;
                holder.querySelectorAll('.att-thumb-only-label, .att-broken-label').forEach((el) => el.remove());
                const label = document.createElement('div');
                label.className = className;
                label.textContent = text;
                holder.appendChild(label);
            };

            const markThumbOnlyLoaded = () => {
                if (!originalAvailable && links?.thumb_available === true && holder && !holder.querySelector('.att-thumb-only-label')) {
                    setOverlayLabel('att-thumb-only-label', 'Оригинал удалён, доступно только превью');
                }
            };

            if (img.dataset.fallbackWired !== '1') {
                img.dataset.fallbackWired = '1';
                img.addEventListener('load', () => {
                    img.classList.remove('is-broken');
                    markThumbOnlyLoaded();
                });
                img.addEventListener('error', () => {
                    const fallback = (img.getAttribute('data-raw-src') || '').toString();
                    let current = '';
                    let next = '';
                    try { current = new URL(img.currentSrc || img.src || '', location.href).href; } catch (_) { current = img.currentSrc || img.src || ''; }
                    try { next = new URL(fallback || '', location.href).href; } catch (_) { next = fallback || ''; }

                    if (originalAvailable && fallback && current !== next && img.dataset.rawTried !== '1') {
                        img.dataset.rawTried = '1';
                        img.src = fallback;
                    } else {
                        img.classList.add('is-broken');
                        setOverlayLabel(
                            'att-broken-label',
                            originalAvailable
                                ? 'Не удалось открыть предпросмотр. Скачивание может работать.'
                                : 'Оригинал удалён. Превью недоступно.'
                        );
                    }
                });
            }

            if (previewUrl) {
                img.src = previewUrl;
                try { img.removeAttribute('data-src'); } catch (_) {}
                if (img.complete && img.naturalWidth > 0) markThumbOnlyLoaded();
            }
        }

        // video
        const v = att.querySelector?.('video.att-video');
        if (v && links.raw_url) {
            if (v.src !== links.raw_url) v.src = links.raw_url;
            v.setAttribute('data-src', links.raw_url);
            try { v.load(); } catch (_) {}
        }

        // audio
        const a = att.querySelector?.('audio.att-audio');
        if (a && links.raw_url) {
            // avoid initial 401: src is set only after we have signed link
            a.src = links.raw_url;
            try { a.removeAttribute('data-src'); } catch (_) {}
        }
    }).catch((err) => {
        if (isGoneError(err)) {
            markAttachmentExpired(att);
        }
    });
});

}


function trimAutoLinkPunctuation(url) {
    let clean = (url || '').toString();
    let tail = '';

    while (/[.,!?;:]$/.test(clean)) {
        tail = clean.slice(-1) + tail;
        clean = clean.slice(0, -1);
    }

    while (/[)\]]$/.test(clean)) {
        const ch = clean.slice(-1);
        const opens = ch === ')' ? (clean.match(/\(/g) || []).length : (clean.match(/\[/g) || []).length;
        const closes = ch === ')' ? (clean.match(/\)/g) || []).length : (clean.match(/\]/g) || []).length;
        if (closes <= opens) break;
        tail = ch + tail;
        clean = clean.slice(0, -1);
    }

    return { clean, tail };
}

function isSafeMessageUrl(url) {
    return /^https?:\/\//i.test((url || '').toString().trim());
}

const MD_HARD_PLAIN_CHARS = 90000;
const MD_HARD_PLAIN_LINES = 1200;
const MD_HARD_MARKERS = 3500;
const MD_MAX_RENDER_CHARS = 14000;
const MD_MAX_RENDER_LINES = 360;
const MD_MAX_INLINE_CHARS = 1200;
const MD_MAX_BLOCKS = 360;
const MD_MAX_LIST_ITEMS = 180;

const MESSAGE_TEXT_FILE_THRESHOLD = 4000;
const TEXT_ATTACHMENT_PREVIEW_BYTES = 64 * 1024;
const TEXT_ATTACHMENT_MAX_RENDER_CHARS = 72 * 1024;


function countMarkdownMarkersFast(src) {
    let n = 0;
    for (let i = 0; i < src.length; i++) {
        const c = src.charCodeAt(i);
        // *, _, ~, `, [, ], #, >, |, newline
        if (c === 42 || c === 95 || c === 126 || c === 96 || c === 91 || c === 93 || c === 35 || c === 62 || c === 124 || c === 10) {
            n++;
            if (n > MD_HARD_MARKERS) return n;
        }
    }
    return n;
}

function shouldRenderMarkdownAsPlain(src) {
    if (!src) return false;
    if (src.length > MD_HARD_PLAIN_CHARS) return true;

    let lines = 1;
    let currentLine = 0;
    let maxLine = 0;
    for (let i = 0; i < src.length; i++) {
        if (src.charCodeAt(i) === 10) {
            lines++;
            if (currentLine > maxLine) maxLine = currentLine;
            currentLine = 0;
            if (lines > MD_HARD_PLAIN_LINES) return true;
        } else {
            currentLine++;
            if (currentLine > 2600) return true;
        }
    }
    if (currentLine > maxLine) maxLine = currentLine;
    if (maxLine > 2600) return true;

    return countMarkdownMarkersFast(src) > MD_HARD_MARKERS;
}

function renderPlainTextFast(text, reason = '') {
    const src = (text ?? '').toString().replace(/\r\n?/g, '\n');
    if (!src) return '';
    const note = reason
        ? `<div class="msg-md-note">${escapeHtml(reason)}</div>`
        : '';
    return `<div class="msg-md msg-md-safe-plain">${note}<pre class="msg-md-plain">${escapeHtml(src)}</pre></div>`;
}

function isTextAttachmentFile(name, mime) {
    const n = (name || '').toString().toLowerCase();
    const m = (mime || '').toString().toLowerCase().split(';')[0].trim();
    if (m.startsWith('text/')) return true;
    if (m === 'application/json' || m === 'application/xml' || m === 'application/yaml' || m === 'application/x-yaml') return true;
    return /\.(md|markdown|txt|log|json|csv|tsv|xml|yaml|yml|sql|rs|js|jsx|ts|tsx|css|scss|html|htm|toml|ini|cfg|conf|env|sh|bash|bat|ps1|py|java|kt|go|c|cpp|h|hpp|cs|php|rb|lua|r|swift|dart)$/i.test(n);
}

function isMarkdownAttachmentFile(name, mime) {
    const n = (name || '').toString().toLowerCase();
    const m = (mime || '').toString().toLowerCase().split(';')[0].trim();
    return m === 'text/markdown' || n.endsWith('.md') || n.endsWith('.markdown');
}

function buildMarkdownAttachmentName(prefix = 'message') {
    const d = new Date();
    const pad = (n) => String(n).padStart(2, '0');
    const stamp = d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate()) + '_' + pad(d.getHours()) + '-' + pad(d.getMinutes()) + '-' + pad(d.getSeconds());
    return prefix + '_' + stamp + '.md';
}

function makeMarkdownFileFromText(text, name) {
    const body = (text ?? '').toString().replace(/\r\n?/g, '\n');
    const safeName = (name || buildMarkdownAttachmentName()).toString().trim() || buildMarkdownAttachmentName();
    const finalName = safeName.toLowerCase().endsWith('.md') ? safeName : (safeName + '.md');

    // Use text/plain for the multipart body for maximum server compatibility.
    // The .md extension is enough for LaBerry UI to treat it as Markdown/text.
    try {
        return new File([body], finalName, { type: 'text/plain', lastModified: Date.now() });
    } catch (_) {
        const blob = new Blob([body], { type: 'application/octet-stream' });
        blob.name = finalName;
        blob.lastModified = Date.now();
        return blob;
    }
}

function applyInlineMarksSafe(escaped) {
    let html = (escaped ?? '').toString();
    if (html.length > MD_MAX_INLINE_CHARS) return html;

    html = html.replace(/\*\*([^*\n]{1,180})\*\*/g, '<strong>$1</strong>');
    html = html.replace(/__([^_\n]{1,180})__/g, '<strong>$1</strong>');
    html = html.replace(/~~([^~\n]{1,180})~~/g, '<del>$1</del>');
    html = html.replace(/(^|[^*])\*([^*\n]{1,120})\*(?!\*)/g, '$1<em>$2</em>');
    html = html.replace(/(^|[^_])_([^_\n]{1,120})_(?!_)/g, '$1<em>$2</em>');
    return html;
}

function renderInlineMarkdownSafe(text) {
    const src = (text ?? '').toString();
    if (!src) return '';
    if (src.length > MD_MAX_INLINE_CHARS) return escapeHtml(src);

    // Один проход по строке. Без тяжёлых regex на длинных пастах.
    const tokenRe = /`([^`\n]{1,260})`|\[([^\]\n]{1,120})\]\((https?:\/\/[^\s)<>]{1,500})\)|(https?:\/\/[^\s<]{1,650})/gi;
    let out = '';
    let pos = 0;
    let guard = 0;

    for (const m of src.matchAll(tokenRe)) {
        guard++;
        if (guard > 24) break;

        const start = m.index ?? 0;
        if (start < pos) continue;

        out += applyInlineMarksSafe(escapeHtml(src.slice(pos, start)));

        if (m[1] !== undefined) {
            out += `<code class="msg-md-code">${escapeHtml(m[1])}</code>`;
            pos = start + m[0].length;
            continue;
        }

        if (m[2] && m[3] && isSafeMessageUrl(m[3])) {
            const safeUrl = m[3].replace(/["'<>\s]/g, '');
            out += `<a class="msg-link" href="${escapeHtml(safeUrl)}" target="_blank" rel="noopener noreferrer">${escapeHtml(m[2])}</a>`;
            pos = start + m[0].length;
            continue;
        }

        const rawUrl = (m[4] || '').toString();
        const { clean, tail } = trimAutoLinkPunctuation(rawUrl);
        const safeUrl = clean.replace(/["'<>\s]/g, '');

        if (isSafeMessageUrl(safeUrl)) {
            out += `<a class="msg-link" href="${escapeHtml(safeUrl)}" target="_blank" rel="noopener noreferrer">${escapeHtml(clean)}</a>${applyInlineMarksSafe(escapeHtml(tail))}`;
        } else {
            out += applyInlineMarksSafe(escapeHtml(rawUrl));
        }

        pos = start + rawUrl.length;
    }

    out += applyInlineMarksSafe(escapeHtml(src.slice(pos)));
    return out;
}


function splitMarkdownTableRow(line) {
    let raw = (line ?? '').toString().trim();
    if (!raw.includes('|')) return [];

    if (raw.startsWith('|')) raw = raw.slice(1);
    if (raw.endsWith('|')) raw = raw.slice(0, -1);

    const cells = [];
    let cur = '';
    let escaped = false;
    for (const ch of raw) {
        if (escaped) {
            cur += ch;
            escaped = false;
            continue;
        }
        if (ch === '\\') {
            escaped = true;
            cur += ch;
            continue;
        }
        if (ch === '|') {
            cells.push(cur.trim());
            cur = '';
            continue;
        }
        cur += ch;
    }
    cells.push(cur.trim());
    return cells;
}

function parseMarkdownTableSeparator(line) {
    const cells = splitMarkdownTableRow(line);
    if (cells.length < 2) return null;

    const aligns = [];
    for (const cell of cells) {
        const c = cell.trim();
        if (!/^:?-{3,}:?$/.test(c)) return null;
        if (c.startsWith(':') && c.endsWith(':')) aligns.push('center');
        else if (c.endsWith(':')) aligns.push('right');
        else aligns.push('left');
    }
    return aligns;
}

function isMarkdownTableStart(lines, index) {
    if (!Array.isArray(lines) || index + 1 >= lines.length) return false;
    const head = splitMarkdownTableRow(lines[index]);
    if (head.length < 2) return false;
    const aligns = parseMarkdownTableSeparator(lines[index + 1]);
    return !!aligns && aligns.length >= 2;
}

function renderMarkdownTableBlock(tableRows, aligns) {
    if (!tableRows.length) return '';
    const header = tableRows[0];
    const body = tableRows.slice(1);
    const colCount = Math.min(Math.max(header.length, aligns.length), 12);
    const alignAttr = (idx) => {
        const a = aligns[idx] || 'left';
        return a === 'center' || a === 'right' ? ` style="text-align:${a}"` : '';
    };
    const normalizeCells = (row) => {
        const out = row.slice(0, colCount);
        while (out.length < colCount) out.push('');
        return out;
    };

    const thead = normalizeCells(header)
        .map((cell, idx) => `<th${alignAttr(idx)}>${renderInlineMarkdownSafe(cell)}</th>`)
        .join('');
    const tbody = body
        .map((row) => `<tr>${normalizeCells(row).map((cell, idx) => `<td${alignAttr(idx)}>${renderInlineMarkdownSafe(cell)}</td>`).join('')}</tr>`)
        .join('');

    return `<div class="msg-md-table-wrap"><table class="msg-md-table"><thead><tr>${thead}</tr></thead><tbody>${tbody}</tbody></table></div>`;
}

function isMarkdownBlockStart(line) {
    return /^\s*```/.test(line)
        || /^\s{0,3}#{1,3}\s+/.test(line)
        || /^\s{0,3}([-*_])\s*\1\s*\1[\s\1]*$/.test(line)
        || /^\s{0,3}>\s?/.test(line)
        || /^\s{0,3}(?:[-*+]\s+|\d{1,4}[.)]\s+)/.test(line);
}

function renderPlainTextPreserveNewlines(text) {
    const src = (text ?? '').toString().replace(/\r\n?/g, '\n');
    if (!src) return '';
    if (shouldRenderMarkdownAsPlain(src)) {
        return renderPlainTextFast(src, 'Длинная Markdown-паста показана как текст, чтобы не подвесить страницу.');
    }
    return `<div class="msg-md"><p class="msg-md-p">${src.split('\n').map(renderInlineMarkdownSafe).join('<br>')}</p></div>`;
}

function renderMarkdownText(text, opts = {}) {
    const srcOriginal = (text ?? '').toString().replace(/\r\n?/g, '\n');
    if (!srcOriginal.trim()) return '';

    try {
        const full = !!opts.full;
        const maxChars = full ? 90000 : MD_MAX_RENDER_CHARS;
        const maxLines = full ? 1200 : MD_MAX_RENDER_LINES;
        let src = srcOriginal;
        let clipped = false;

        if (src.length > maxChars) {
            src = src.slice(0, maxChars);
            clipped = true;
        }

        let lines = src.split('\n');
        if (lines.length > maxLines) {
            lines = lines.slice(0, maxLines);
            clipped = true;
        }

        const blocks = [];
        let i = 0;

        const pushBlock = (html) => {
            if (blocks.length < MD_MAX_BLOCKS) blocks.push(html);
        };

        const skipBlank = () => {
            while (i < lines.length && !lines[i].trim()) i++;
        };

        while (i < lines.length && blocks.length < MD_MAX_BLOCKS) {
            skipBlank();
            if (i >= lines.length) break;

            const line = lines[i];

            const fence = line.match(/^\s*```+\s*([^`\s]{0,32})?.*$/i);
            if (fence) {
                const lang = (fence[1] || '').trim();
                i++;
                const code = [];
                let codeChars = 0;
                const maxCodeChars = full ? 40000 : 9000;
                while (i < lines.length && !/^\s*```+\s*$/.test(lines[i])) {
                    const ln = lines[i];
                    codeChars += ln.length + 1;
                    if (codeChars <= maxCodeChars) code.push(ln);
                    i++;
                }
                if (i < lines.length && /^\s*```+\s*$/.test(lines[i])) i++;
                if (codeChars > maxCodeChars) code.push('…код обрезан для безопасности интерфейса');
                pushBlock(`<pre class="msg-md-pre"${lang ? ` data-lang="${escapeHtml(lang)}"` : ''}><code>${escapeHtml(code.join('\n'))}</code></pre>`);
                continue;
            }

            const h = line.match(/^\s{0,3}(#{1,6})\s+(.+?)\s*#*\s*$/);
            if (h) {
                const level = Math.min(3, h[1].length);
                pushBlock(`<h${level} class="msg-md-h msg-md-h${level}">${renderInlineMarkdownSafe(h[2])}</h${level}>`);
                i++;
                continue;
            }

            if (/^\s{0,3}([-*_])\s*\1\s*\1[\s\1]*$/.test(line)) {
                pushBlock('<hr class="msg-md-hr">');
                i++;
                continue;
            }

            if (isMarkdownTableStart(lines, i)) {
                const aligns = parseMarkdownTableSeparator(lines[i + 1]) || [];
                const tableRows = [splitMarkdownTableRow(lines[i])];
                i += 2;
                const maxTableRows = full ? 160 : 60;
                while (i < lines.length && lines[i].trim() && lines[i].includes('|') && tableRows.length < maxTableRows) {
                    const cells = splitMarkdownTableRow(lines[i]);
                    if (cells.length < 2) break;
                    tableRows.push(cells);
                    i++;
                }
                while (i < lines.length && lines[i].trim() && lines[i].includes('|')) i++;
                pushBlock(renderMarkdownTableBlock(tableRows, aligns));
                continue;
            }

            if (/^\s{0,3}>\s?/.test(line)) {
                const q = [];
                while (i < lines.length && q.length < 80 && /^\s{0,3}>\s?/.test(lines[i])) {
                    q.push(lines[i].replace(/^\s{0,3}>\s?/, ''));
                    i++;
                }
                while (i < lines.length && /^\s{0,3}>\s?/.test(lines[i])) i++;
                pushBlock(`<blockquote class="msg-md-quote">${q.map(renderInlineMarkdownSafe).join('<br>')}</blockquote>`);
                continue;
            }

            if (/^\s{0,3}[-*+]\s+/.test(line)) {
                const items = [];
                while (i < lines.length && items.length < MD_MAX_LIST_ITEMS) {
                    const m = lines[i].match(/^\s{0,3}[-*+]\s+(.+)$/);
                    if (!m) break;
                    items.push(`<li>${renderInlineMarkdownSafe(m[1])}</li>`);
                    i++;
                }
                while (i < lines.length && /^\s{0,3}[-*+]\s+/.test(lines[i])) i++;
                pushBlock(`<ul class="msg-md-list">${items.join('')}</ul>`);
                continue;
            }

            if (/^\s{0,3}\d{1,4}[.)]\s+/.test(line)) {
                const items = [];
                while (i < lines.length && items.length < MD_MAX_LIST_ITEMS) {
                    const m = lines[i].match(/^\s{0,3}\d{1,4}[.)]\s+(.+)$/);
                    if (!m) break;
                    items.push(`<li>${renderInlineMarkdownSafe(m[1])}</li>`);
                    i++;
                }
                while (i < lines.length && /^\s{0,3}\d{1,4}[.)]\s+/.test(lines[i])) i++;
                pushBlock(`<ol class="msg-md-list">${items.join('')}</ol>`);
                continue;
            }

            const para = [];
            let paraChars = 0;
            while (i < lines.length && lines[i].trim() && !isMarkdownBlockStart(lines[i])) {
                paraChars += lines[i].length + 1;
                if (para.length < 80 && paraChars <= (full ? 9000 : 4500)) para.push(lines[i]);
                i++;
            }

            if (para.length) {
                pushBlock(`<p class="msg-md-p">${para.map(renderInlineMarkdownSafe).join('<br>')}</p>`);
                continue;
            }

            i++;
        }

        if (clipped || i < lines.length || blocks.length >= MD_MAX_BLOCKS) {
            pushBlock('<div class="msg-md-note">Показан форматированный фрагмент. Полный текст можно открыть кнопкой рядом с сообщением/файлом.</div>');
        }

        return `<div class="msg-md">${blocks.join('')}</div>`;
    } catch (err) {
        console.warn('[Markdown] fallback to plain text', err);
        return renderPlainTextFast(srcOriginal, 'Markdown не обработан: показан обычный текст.');
    }
}

function normalizeUrlForPreview(rawUrl) {
    try {
        const { clean } = trimAutoLinkPunctuation((rawUrl || '').toString().trim());
        if (!isSafeMessageUrl(clean)) return null;
        const u = new URL(clean, window.location.href);
        if (!/^https?:$/i.test(u.protocol)) return null;
        return u;
    } catch (_) {
        return null;
    }
}

function extractMessageUrls(rawText) {
    const LINK_PREVIEW_SCAN_CHARS = 16000;
    const src = (rawText || '').toString();
    const scan = src.length > LINK_PREVIEW_SCAN_CHARS ? src.slice(0, LINK_PREVIEW_SCAN_CHARS) : src;
    const cleaned = scan
        .replace(/\[\[file[:=]\d+\|[^\]]*\]\]/g, ' ')
        .replace(/\[\[file:\d+\]\][^\]]*\]\]/g, ' ');
    const out = [];
    const seen = new Set();
    const re = /https?:\/\/[^\s<>"']{3,1200}/gi;
    let m;
    while ((m = re.exec(cleaned)) !== null && out.length < 4) {
        const u = normalizeUrlForPreview(m[0]);
        if (!u) continue;
        const key = u.href;
        if (seen.has(key)) continue;
        seen.add(key);
        out.push(u);
    }
    return out;
}

function youtubeVideoIdFromUrl(u) {
    const host = u.hostname.toLowerCase().replace(/^www\./, '');
    if (host === 'youtu.be') {
        const id = u.pathname.split('/').filter(Boolean)[0] || '';
        return /^[a-zA-Z0-9_-]{6,32}$/.test(id) ? id : null;
    }
    if (host === 'youtube.com' || host === 'm.youtube.com' || host === 'music.youtube.com' || host === 'youtube-nocookie.com') {
        const v = u.searchParams.get('v');
        if (v && /^[a-zA-Z0-9_-]{6,32}$/.test(v)) return v;
        const parts = u.pathname.split('/').filter(Boolean);
        const known = ['shorts', 'embed', 'live', 'v'];
        if (known.includes(parts[0]) && parts[1] && /^[a-zA-Z0-9_-]{6,32}$/.test(parts[1])) return parts[1];
    }
    return null;
}

function rutubeVideoIdFromUrl(u) {
    const host = u.hostname.toLowerCase().replace(/^www\./, '');
    if (host !== 'rutube.ru') return null;
    const parts = u.pathname.split('/').filter(Boolean);
    const idx = parts.indexOf('video');
    const id = idx >= 0 ? parts[idx + 1] : null;
    return id && /^[a-zA-Z0-9_-]{8,80}$/.test(id) ? id : null;
}

function vimeoVideoIdFromUrl(u) {
    const host = u.hostname.toLowerCase().replace(/^www\./, '');
    if (host !== 'vimeo.com' && host !== 'player.vimeo.com') return null;
    const parts = u.pathname.split('/').filter(Boolean);
    const id = parts.find((p) => /^\d{5,20}$/.test(p));
    return id || null;
}

function googleDriveFileIdFromUrl(u) {
    const host = u.hostname.toLowerCase();
    if (!host.endsWith('drive.google.com') && !host.endsWith('docs.google.com')) return null;

    const byQuery = u.searchParams.get('id');
    if (byQuery && /^[a-zA-Z0-9_-]{10,200}$/.test(byQuery)) return byQuery;

    const parts = u.pathname.split('/').filter(Boolean);
    const dIdx = parts.indexOf('d');
    if (dIdx >= 0 && parts[dIdx + 1] && /^[a-zA-Z0-9_-]{10,200}$/.test(parts[dIdx + 1])) {
        return parts[dIdx + 1];
    }

    return null;
}

function githubInfoFromUrl(u) {
    const host = u.hostname.toLowerCase().replace(/^www\./, '');
    if (host !== 'github.com') return null;
    const parts = u.pathname.split('/').filter(Boolean);
    if (parts.length < 2) return null;

    const repo = `${parts[0]}/${parts[1]}`;
    let detail = 'Репозиторий GitHub';
    if (parts[2] === 'issues' && parts[3]) detail = `Issue #${parts[3]}`;
    else if (parts[2] === 'pull' && parts[3]) detail = `Pull request #${parts[3]}`;
    else if (parts[2]) detail = parts.slice(2).join('/');

    return { repo, detail };
}

function providerNameForUrl(u) {
    const host = u.hostname.toLowerCase().replace(/^www\./, '');
    if (host.includes('youtube.com') || host === 'youtu.be') return 'YouTube';
    if (host.includes('rutube.ru')) return 'RuTube';
    if (host.includes('vimeo.com')) return 'Vimeo';
    if (host.includes('drive.google.com') || host.includes('docs.google.com')) return 'Google Drive';
    if (host.includes('disk.yandex.') || host.includes('yadi.sk')) return 'Яндекс Диск';
    if (host.includes('yandex.')) return 'Яндекс';
    return host;
}

function linkHostText(u) {
    try { return u.hostname.replace(/^www\./, ''); } catch (_) { return 'внешний сайт'; }
}

function renderExternalWarningCard(u, opts = {}) {
    const provider = opts.provider || providerNameForUrl(u);
    const title = opts.title || 'Внешняя ссылка';
    const hint = opts.hint || 'Предпросмотр недоступен. Проверь адрес перед переходом.';
    const href = escapeHtml(u.href);
    const host = escapeHtml(linkHostText(u));

    return `
      <div class="link-embed link-warning-card">
        <div class="link-card-mark">!</div>
        <div class="link-card-main">
          <div class="link-card-provider">${escapeHtml(provider)}</div>
          <div class="link-card-title">${escapeHtml(title)}</div>
          <div class="link-card-url">${host}</div>
          <div class="link-card-hint">${escapeHtml(hint)}</div>
          <div class="link-card-actions">
            <a class="link-card-open external-confirm" href="${href}" target="_blank" rel="noopener noreferrer" data-external-url="${href}" data-external-provider="${escapeHtml(provider)}">Перейти</a>
          </div>
        </div>
      </div>
    `;
}

function renderKnownLinkEmbed(u) {
    const ytId = youtubeVideoIdFromUrl(u);
    if (ytId) {
        const thumb = `https://i.ytimg.com/vi/${encodeURIComponent(ytId)}/hqdefault.jpg`;
        return `
          <div class="link-embed link-video-embed link-video-summary" data-provider="youtube">
            <div class="link-embed-head">
              <span class="link-provider">YouTube</span>
              <a class="link-open-direct" href="${escapeHtml(u.href)}" target="_blank" rel="noopener noreferrer">Открыть</a>
            </div>
            <a class="link-thumb-wrap" href="${escapeHtml(u.href)}" target="_blank" rel="noopener noreferrer">
              <img class="link-thumb" src="${escapeHtml(thumb)}" alt="YouTube preview" loading="lazy">
              <span class="link-play-badge" aria-hidden="true">▶</span>
            </a>
          </div>
        `;
    }

    const rtId = rutubeVideoIdFromUrl(u);
    if (rtId) {
        const embed = `https://rutube.ru/play/embed/${encodeURIComponent(rtId)}`;
        return `
          <div class="link-embed link-video-embed" data-provider="rutube">
            <div class="link-embed-head">
              <span class="link-provider">RuTube</span>
              <a class="link-open-direct" href="${escapeHtml(u.href)}" target="_blank" rel="noopener noreferrer">Открыть</a>
            </div>
            <div class="link-frame-wrap">
              <iframe class="link-frame" src="${escapeHtml(embed)}" title="RuTube video" loading="lazy" allow="clipboard-write; encrypted-media; fullscreen; picture-in-picture" allowfullscreen referrerpolicy="strict-origin-when-cross-origin"></iframe>
            </div>
          </div>
        `;
    }

    const vmId = vimeoVideoIdFromUrl(u);
    if (vmId) {
        const embed = `https://player.vimeo.com/video/${encodeURIComponent(vmId)}`;
        return `
          <div class="link-embed link-video-embed" data-provider="vimeo">
            <div class="link-embed-head">
              <span class="link-provider">Vimeo</span>
              <a class="link-open-direct" href="${escapeHtml(u.href)}" target="_blank" rel="noopener noreferrer">Открыть</a>
            </div>
            <div class="link-frame-wrap">
              <iframe class="link-frame" src="${escapeHtml(embed)}" title="Vimeo video" loading="lazy" allow="autoplay; fullscreen; picture-in-picture; clipboard-write" allowfullscreen referrerpolicy="strict-origin-when-cross-origin"></iframe>
            </div>
          </div>
        `;
    }

    const driveId = googleDriveFileIdFromUrl(u);
    if (driveId) {
        // Google Drive often blocks iframe preview for private/limited files.
        // A compact warning card is safer than a large broken frame.
        return renderExternalWarningCard(u, {
            provider: 'Google Drive',
            title: 'Внешний файл Google Drive',
            hint: 'Предпросмотр зависит от доступа к файлу. Открывай только если доверяешь отправителю.'
        });
    }

    const host = u.hostname.toLowerCase();
    const gh = githubInfoFromUrl(u);
    if (gh) {
        return `
          <div class="link-embed link-summary-card" data-provider="github">
            <div class="link-card-mark">GH</div>
            <div class="link-card-main">
              <div class="link-card-provider">github.com</div>
              <div class="link-card-title">${escapeHtml(gh.repo)}</div>
              <div class="link-card-url">${escapeHtml(gh.detail)}</div>
              <div class="link-card-actions">
                <a class="link-card-open" href="${escapeHtml(u.href)}" target="_blank" rel="noopener noreferrer">Открыть</a>
              </div>
            </div>
          </div>
        `;
    }

    if (host.includes('disk.yandex.') || host.includes('yadi.sk') || host.includes('yandex.')) {
        return renderExternalWarningCard(u, {
            provider: providerNameForUrl(u),
            title: 'Внешний контент',
            hint: 'Авто-предпросмотр для этой ссылки не включён. Перейди вручную, если доверяешь отправителю.'
        });
    }

    return renderExternalWarningCard(u);
}

function renderLinkEmbedsForMessage(rawText) {
    const urls = extractMessageUrls(rawText);
    if (!urls.length) return '';

    const cards = [];
    for (const u of urls) {
        const card = renderKnownLinkEmbed(u);
        if (card) cards.push(card);
    }

    if (!cards.length) return '';
    return `<div class="message-link-embeds">${cards.join('')}</div>`;
}

let externalGuardReady = false;

function setupExternalLinkGuards() {
    if (externalGuardReady) return;
    externalGuardReady = true;

    document.addEventListener('click', (e) => {
        const link = e.target?.closest?.('a.external-confirm');
        if (!link) return;

        const url = (link.dataset.externalUrl || link.href || '').toString();
        const provider = (link.dataset.externalProvider || 'внешний сайт').toString();
        if (!url) return;

        const ok = window.confirm(`Открыть внешнюю ссылку (${provider})?\n\n${url}\n\nLaBerry не контролирует этот сайт.`);
        if (!ok) {
            e.preventDefault();
            e.stopPropagation();
        }
    }, true);
}


function ensureMarkdownFullViewer() {
    let overlay = document.getElementById('markdownFullViewer');
    if (overlay) return overlay;

    overlay = document.createElement('div');
    overlay.id = 'markdownFullViewer';
    overlay.className = 'modal-overlay hidden markdown-full-overlay';
    overlay.innerHTML = `
      <div class="markdown-full-dialog" role="dialog" aria-modal="true" aria-label="Полный Markdown">
        <div class="markdown-full-head">
          <div>
            <div class="markdown-full-title">Markdown</div>
            <div class="markdown-full-sub" data-md-full-meta></div>
          </div>
          <button type="button" class="modal-close" data-md-full-close>✕</button>
        </div>
        <div class="markdown-full-body" data-md-full-body></div>
      </div>
    `;
    document.body.appendChild(overlay);

    const close = () => overlay.classList.add('hidden');
    overlay.addEventListener('click', (e) => {
        if (e.target === overlay || e.target.closest('[data-md-full-close]')) close();
    });
    document.addEventListener('keydown', (e) => {
        if (e.key === 'Escape' && !overlay.classList.contains('hidden')) close();
    });
    return overlay;
}


function openFullMarkdownText(title, raw, metaText = '') {
    const overlay = ensureMarkdownFullViewer();
    const body = overlay.querySelector('[data-md-full-body]');
    const meta = overlay.querySelector('[data-md-full-meta]');
    const titleEl = overlay.querySelector('.markdown-full-title');
    const src = (raw ?? '').toString();
    if (titleEl) titleEl.textContent = title || 'Markdown';
    if (body) body.innerHTML = renderMarkdownText(src, { full: true });
    if (meta) meta.textContent = metaText || `${src.split('\n').length} строк · ${formatBytes(new Blob([src]).size)}`;
    overlay.classList.remove('hidden');
}

function openFullMarkdownMessage(messageId) {
    const id = Number(messageId);
    if (!Number.isFinite(id) || id <= 0) return;
    const cached = window.__lbMsgCache?.get?.(id);
    const raw = (cached?.content || '').toString();
    if (!raw) return;

    const overlay = ensureMarkdownFullViewer();
    const body = overlay.querySelector('[data-md-full-body]');
    const meta = overlay.querySelector('[data-md-full-meta]');
    if (meta) meta.textContent = `Сообщение #${id} · ${raw.split('\n').length} строк · ${formatBytes(new Blob([raw]).size)}`;
    if (body) body.innerHTML = renderMarkdownText(raw, { full: true });
    overlay.classList.remove('hidden');
}

if (!window.__lbMarkdownFullWired) {
    window.__lbMarkdownFullWired = true;
    document.addEventListener('click', (e) => {
        const btn = e.target?.closest?.('.msg-md-open-full');
        if (!btn) return;
        e.preventDefault();
        e.stopPropagation();
        openFullMarkdownMessage(btn.dataset.msgId);
    });
}

function shouldRenderLinkEmbedsForMessage(rawText) {
    const src = (rawText || '').toString();
    if (!src) return false;
    if (shouldRenderMarkdownAsPlain(src)) return false;
    if (src.length > 2800) return false;

    let lines = 1;
    for (let i = 0; i < src.length; i++) {
        if (src.charCodeAt(i) === 10) {
            lines++;
            if (lines > 70) return false;
        }
    }

    return true;
}


function renderMessageContent(content) {
    const raw = (content ?? '').toString();
    if (e2eeIsEncryptedText(raw)) {
        return `<span class="encrypted-message">🔒 Зашифрованное сообщение</span>`;
    }

    // Support both canonical and broken legacy markers.
    // canonical: [[file:ID|NAME|MIME|SIZE]]
    // broken:    [[file:ID]]NAME|MIME|SIZE]]
    const reAny = /\[\[file[:=](\d+)\|([^|]*)\|([^|]*)\|(\d+)\]\]|\[\[file[:=](\d+)\]\]([^|\]]*)\|([^|\]]*)\|(\d+)\]\]/g;
    if (!reAny.test(raw)) {
        const body = renderMarkdownText(raw);
        const embeds = shouldRenderLinkEmbedsForMessage(raw) ? renderLinkEmbedsForMessage(raw) : '';
        return body + embeds;
    }

    reAny.lastIndex = 0;

    const dlSvg = `
      <svg class="dl-ico" viewBox="0 0 24 24" aria-hidden="true">
        <path d="M12 3v10m0 0l-4-4m4 4l4-4M4 17v3h16v-3"
              fill="none" stroke="currentColor" stroke-width="3.2" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
    `;

    const playSvg = `
      <svg class="play-ico" viewBox="0 0 24 24" aria-hidden="true">
        <path d="M8 6.5v11a1 1 0 0 0 1.53.85l8.6-5.5a1 1 0 0 0 0-1.7l-8.6-5.5A1 1 0 0 0 8 6.5Z"
              fill="currentColor"/>
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
        out += renderMarkdownText(raw.slice(last, start));

        // canonical groups: 1-4, broken groups: 5-8
        const isCanonical = m[1] !== undefined && m[1] !== null;
        const id = isCanonical ? m[1] : m[5];
        const encName = isCanonical ? (m[2] || '') : (m[6] || '');
        const mime = ((isCanonical ? (m[3] || '') : (m[7] || '')) || '').toLowerCase();
        const size = isCanonical ? m[4] : m[8];

        let name = '';
        try { name = decodeURIComponent(encName); } catch (_) { name = encName; }
        if (!name) name = `file_${id}`;

        const lowerName = (name || '').toString().toLowerCase();
        const isGif = mime === 'image/gif' || lowerName.endsWith('.gif');
        const isImage = (mime.startsWith('image/') && mime !== 'image/svg+xml') || /\.(png|jpe?g|gif|webp|bmp|avif)$/i.test(lowerName);
        const isVideo = mime.startsWith('video/') || /\.(mp4|webm|mov|m4v|avi|mkv)$/i.test(lowerName);
        const isAudio = mime.startsWith('audio/') || /\.(mp3|wav|ogg|m4a|flac|aac)$/i.test(lowerName);
        const isMedia = isImage || isVideo || isAudio;
        const isTextDoc = (!isMedia) && isTextAttachmentFile(name, mime);
        const isArchive = (!isMedia) && (
            mime === 'application/zip' || mime === 'application/x-zip-compressed' ||
            lowerName.endsWith('.zip') || lowerName.endsWith('.rar') || lowerName.endsWith('.7z') ||
            lowerName.endsWith('.tar') || lowerName.endsWith('.gz') || lowerName.endsWith('.tgz')
        );
        const sizeText = formatBytes(size);

        const href = '#'; // resolved on click via signed link
        const rawHref = `/api/files/${encodeURIComponent(id)}/raw`; // inline / stream
        const previewHref = isGif ? rawHref : `/api/files/${encodeURIComponent(id)}/preview`;
        const badge = fileBadge(name, mime);
        const mediaKindClass = isGif ? 'media-image media-gif gif-sticker' : isImage ? 'media-image' : isVideo ? 'media-video' : isAudio ? 'media-audio' : 'file-generic';

        const attData = `data-file-id="${escapeHtml(id)}" data-file-name="${escapeHtml(name)}" data-file-mime="${escapeHtml(mime)}" data-file-size="${escapeHtml(size)}"`;
        const gifFavoriteBtn = isGif
            ? `<button type="button" class="att-fav" title="Добавить GIF в избранное" aria-label="Добавить GIF в избранное">☆</button>`
            : '';

        const previewHtml = isImage
            ? `<div class="att-preview att-preview-image" tabindex="0" role="button" aria-label="Открыть изображение ${escapeHtml(name)}">${gifFavoriteBtn}<img class="att-img" src="data:image/gif;base64,R0lGODlhAQABAAAAACwAAAAAAQABAAA=" data-src="${previewHref}" data-raw-src="${rawHref}" alt="${escapeHtml(name)}" loading="lazy" decoding="async"><a class="att-dl" href="${href}" download title="Скачать">${dlSvg}</a></div>`
            : isVideo
                ? `<div class="att-preview att-preview-video"><video class="att-video" preload="metadata" playsinline></video><button type="button" class="att-play" title="Воспроизвести" aria-label="Воспроизвести">${playSvg}</button><a class="att-dl" href="${href}" download title="Скачать">${dlSvg}</a></div>`
                : isAudio
                    ? `<div class="att-preview att-preview-audio"><div class="att-audio-shell"><div class="att-audio-icon" aria-hidden="true">♫</div><div class="att-audio-main"><div class="att-audio-title" title="${escapeHtml(name)}">${escapeHtml(name)}</div><div class="att-audio-sub">${escapeHtml(badge)}${sizeText ? ` • ${escapeHtml(sizeText)}` : ''}</div><audio class="att-audio" controls preload="metadata"></audio></div></div><a class="att-dl" href="${href}" download title="Скачать">${dlSvg}</a></div>`
                    : '';

        const rowHtml = isMedia
            ? ''
            : `<div class="file-row"><span class="file-badge">${escapeHtml(badge)}</span><span class="file-name" title="${escapeHtml(name)}">${escapeHtml(name)}</span><span class="file-meta">${sizeText ? escapeHtml(sizeText) : ''}</span>${isTextDoc ? `<button type="button" class="att-text-toggle" title="Развернуть текст">Открыть</button>` : ''}${isArchive ? `<button type="button" class="att-archive" data-act="archive" title="Посмотреть содержимое">📦</button>` : ''}<a class="att-dl" href="${href}" download title="Скачать">${dlSvg}</a></div>`;

        out += `<div class="msg-attachment ${isMedia ? 'media' : 'file'} ${mediaKindClass} ${isTextDoc ? 'text-doc' : ''} ${isVideo ? 'hover-dl' : ''}" ${attData}>${previewHtml}${rowHtml}</div>`;

        last = start + m[0].length;
    }

    out += renderMarkdownText(raw.slice(last));
    if (shouldRenderLinkEmbedsForMessage(raw)) {
        out += renderLinkEmbedsForMessage(raw);
    }
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

    try {
        const midForFull = Number(msg?.id);
        const rawForFull = (msg?.content || '').toString();
        if (Number.isFinite(midForFull) && midForFull > 0 && rawForFull.length > 3500 && !rawForFull.includes('[[file:')) {
            div.querySelector('.text')?.insertAdjacentHTML('beforeend', `<button type="button" class="msg-md-open-full" data-msg-id="${escapeHtml(midForFull)}">Открыть полную Markdown-пасту</button>`);
        }
    } catch (_) {}

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
    wireAvatarFallbacks(div);

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
            btn.disabled = true;
            btn.classList.add('is-loading');

            try {
                await api(`/api/messages/${mid}`, { method: 'DELETE' });

                document.querySelectorAll(`.message[data-msg-id="${mid}"]`).forEach((el) => {
                    try { el.remove(); } catch (_) {}
                });

                showToast('Сообщение удалено');
            } catch (err) {
                console.warn('[UI] delete message failed', err);
                const status = err?.status ? ` (${err.status})` : '';
                const msgText = (err?.message || '').toString();
                const detailMatch = msgText.match(/\{\s*"detail"\s*:\s*"([^"]+)"\s*\}/);
                const detail = detailMatch ? `: ${detailMatch[1]}` : '';
                showToast(`Не удалось удалить сообщение${status}${detail}`);
            } finally {
                btn.disabled = false;
                btn.classList.remove('is-loading');
            }
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
    
    appLog('[APP] Initializing...');
    isInitialized = true;
    
    const overlay = $("loading-overlay");
    if (overlay) overlay.classList.remove("hidden");

    try {
        normalizeHash();
        applyTheme(localStorage.getItem('theme') || 'dark');
        registerNotificationWorker();
        await loadMe();
        await ensureCookieConsentFlow();
        try {
            await e2eeEnsureIdentity(true);
        } catch (e) {
            console.warn('[E2EE] identity setup failed', e);
        }
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
        document.getElementById('addChannelBtn')?.addEventListener('click', (e) => { e.preventDefault(); e.stopPropagation(); openCurrentServerMenu(e.currentTarget); });
        document.getElementById('dmCallBtn')?.addEventListener('click', (e) => { e.preventDefault(); e.stopPropagation(); startDmCall(); });
        document.getElementById('profilePanelToggleBtn')?.addEventListener('click', (e) => { e.preventDefault(); e.stopPropagation(); setDmProfilePanelHidden(false); });
        document.getElementById('membersPanelHideBtn')?.addEventListener('click', (e) => { e.preventDefault(); e.stopPropagation(); setDmProfilePanelHidden(true); });
        setupServerSearch();
        setupDmHomeMenu();
        document.addEventListener('click', (e) => {
            const t = e.target;
            if (t && t.id === 'addServerBtn') {
                e.preventDefault();
                openServerHubModal('create');
            }
        });

        // mobile drawer buttons
        document.getElementById('mobileServersBtn')?.addEventListener('click', (e) => {
            e.preventDefault();
            e.stopPropagation();
            if (document.body.classList.contains('servers-open')) {
                hideServersMenu();
                return;
            }
            hideChannelsMenu();
            hideMembersMenu();
            showServersMenu();
        });

        document.getElementById('friendsBtn')?.addEventListener('click', (e) => {
            if (!isTouchUi()) return;
            e.preventDefault();
            e.stopPropagation();
            showMobileDmDrawer().catch((err) => console.warn('[UI] mobile friends drawer failed', err));
        });


        document.getElementById('mobileMembersBtn')?.addEventListener('click', (e) => {
            e.preventDefault();
            e.stopPropagation();
            toggleMembersMenu();
        });

        document.getElementById('uiOverlay')?.addEventListener('click', () => {
            closeAllDrawers();
        });

        document.addEventListener('keydown', (ev) => {
            if (ev.key === 'Escape' && document.body.classList.contains('drawer-open')) {
                closeAllDrawers();
            }
        });


        setupWebSocketHandlers();
        setupMessageComposer();
        setupMessagesInfiniteScroll();
        setupExternalLinkGuards();
        wireMessagesAutoScroll();
        ensureJumpToPresentBtn();
        updateJumpBtn();
        setupAttachmentUi();
        // mobile drawers are closed by clicking overlay
        const servers = await api("/api/servers");
        appLog('[APP] Servers loaded:', servers);
        
        const lastServerId = Number(sessionStorage.getItem("lastServerId"));
        const lastChatId = Number(sessionStorage.getItem("lastChatId"));
        
        appLog('[APP] Restoring from sessionStorage:', {
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
            appLog('[APP] Chats loaded:', chats);
            
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
    appLog('[APP] DOM loaded, checking auth...');
    
    const token = localStorage.getItem("auth_token");
    if (!token) {
        appLog('[APP] No auth token, redirecting to login...');
        window.location.href = "/";
        return;
    }
    
    appLog('[APP] Auth token found, initializing...');
    initializeApp();
});

window.addEventListener('resize', () => {
    if (window.innerWidth > 900) {
        hideChannelsMenu();
    }
    applyDmProfilePanelVisibility();
    refreshDmCallFloat();
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

appLog('[APP] Application script loaded successfully');
