import { api } from "./api.js?v=7";
import { openAvatarCropper } from "./avatar-cropper.js?v=7";

const $ = (id) => document.getElementById(id);

const DEFAULT_SETTINGS = {
    theme: 'dark',
    locale: 'ru',
    show_header_status: false,
    compact_mode: false,
    show_timestamps: true,
    font_scale: 1.0,
    connections: [],
    friend_requests: 'everyone',
    dms: 'friends_and_server',
    notify_desktop: true,
    notify_sounds: true,
    notify_dms: true,
    notify_mentions: true,
    developer_mode: false,
};

function normalizeSettings(s) {
    const v = { ...DEFAULT_SETTINGS, ...(s || {}) };

    v.theme = (v.theme || 'dark').toString().toLowerCase() === 'light' ? 'light' : 'dark';
    v.locale = (v.locale || 'ru').toString().toLowerCase() === 'en' ? 'en' : 'ru';

    const fr = (v.friend_requests || 'everyone').toString().toLowerCase();
    v.friend_requests = ['everyone', 'friends_of_friends', 'server_members', 'none'].includes(fr) ? fr : 'everyone';

    const dm = (v.dms || 'friends_and_server').toString().toLowerCase();
    v.dms = ['friends_only', 'friends_and_server', 'everyone'].includes(dm) ? dm : 'friends_and_server';
    v.show_header_status = !!v.show_header_status;
    v.compact_mode = !!v.compact_mode;
    v.show_timestamps = !!v.show_timestamps;
    v.notify_desktop = !!v.notify_desktop;
    v.notify_sounds = !!v.notify_sounds;
    v.notify_dms = !!v.notify_dms;
    v.notify_mentions = !!v.notify_mentions;
    v.developer_mode = !!v.developer_mode;


    let fs = Number(v.font_scale);
    if (Number.isNaN(fs) || !Number.isFinite(fs)) fs = 1.0;
    if (fs < 0.8) fs = 0.8;
    if (fs > 1.3) fs = 1.3;
    v.font_scale = fs;

    const rawConns = Array.isArray(v.connections) ? v.connections : [];
    v.connections = rawConns
        .filter(Boolean)
        .slice(0, 12)
        .map((c) => {
            const kind = (c?.kind || 'other').toString().toLowerCase();
            const allowed = ['discord', 'telegram', 'github', 'youtube', 'twitch', 'website', 'other'];
            const safeKind = allowed.includes(kind) ? kind : 'other';

            let url = (c?.url || '').toString().trim();
            if (url && !/^https?:\/\//i.test(url)) url = `https://${url}`;
            if (url.length > 2048) url = url.slice(0, 2048);

            let label = (c?.label ?? '').toString().trim();
            if (!label) label = '';
            if (label.length > 64) label = label.slice(0, 64);

            return {
                kind: safeKind,
                url,
                label: label || undefined,
            };
        })
        .filter((c) => !!c.url);


    return v;
}

function applyUiSettings(settings, applyТема) {
    const s = normalizeSettings(settings);

    if (typeof applyТема === 'function') {
        applyТема(s.theme);
    } else {
        const root = document.documentElement;
        root.classList.remove('theme-dark', 'theme-light');
        root.classList.add(s.theme === 'light' ? 'theme-light' : 'theme-dark');
        localStorage.setItem('theme', s.theme);
    }

    document.body.classList.toggle('hide-header-status', !s.show_header_status);
    document.body.classList.toggle('compact-mode', !!s.compact_mode);
    document.body.classList.toggle('hide-timestamps', !s.show_timestamps);

    try {
        document.documentElement.style.setProperty('--ui-scale', String(s.font_scale || 1));
    } catch (_) {}
    const __sc = Number(s.font_scale || 1);
    const __scaled = Number.isFinite(__sc) && Math.abs(__sc - 1) > 0.001;
    try { document.documentElement.classList.toggle('ui-scaled', __scaled); } catch (_) {}
    try { document.body.classList.toggle('ui-scaled', __scaled); } catch (_) {}
    try { localStorage.setItem('ui_scale', String(s.font_scale || 1)); } catch (_) {}

}

function escapeHtml(s) {
    return String(s ?? '')
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#39;');
}

function mountOverlay() {
    let overlay = $("settingsOverlay");
    if (overlay) return overlay;

    overlay = document.createElement('div');
    overlay.id = 'settingsOverlay';
    overlay.className = 'modal-overlay hidden';

    overlay.innerHTML = `
      <div class="settings-modal" role="dialog" aria-modal="true">
        <div class="settings-sidebar">
          <div class="settings-side-header">Пользовательские настройки</div>
          <button class="settings-nav-item active" data-section="account" type="button">Мой аккаунт</button>
          <button class="settings-nav-item" data-section="privacy" type="button">Конфиденциальность</button>
          <button class="settings-nav-item" data-section="notifications" type="button">Уведомления</button>
          <div class="settings-side-sep"></div>
          <div class="settings-side-header">Настройки приложения</div>
          <button class="settings-nav-item" data-section="appearance" type="button">Внешний вид</button>
          <button class="settings-nav-item" data-section="connections" type="button">Интеграции</button>
          <button class="settings-nav-item" data-section="keybinds" type="button">Клавиши</button>
          <button class="settings-nav-item" data-section="advanced" type="button">Расширенные</button>
          <div class="settings-side-sep"></div>
          <button class="settings-nav-item danger" data-section="logout" type="button">Выйти</button>
        </div>
        <div class="settings-content">
          <div class="settings-topbar">
            <div>
              <div class="settings-title" id="settingsTitle">Настройки</div>
            </div>
            <button class="settings-close" id="settingsCloseBtn" type="button" title="Закрыть">✕</button>
          </div>
          <div class="settings-body" id="settingsBody"></div>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    return overlay;
}

export function createSettingsUI(opts = {}) {
    const applyТема = opts.applyТема;
    const applyMyStatusToUI = opts.applyMyStatusToUI;
    const updateMyStatus = opts.updateMyStatus;
    const getCurrentUser = opts.getCurrentUser || (() => null);
    const setCurrentUser = opts.setCurrentUser || (() => {});

    let overlay = null;
    let activeSection = 'account';

    let currentSettings = { ...DEFAULT_SETTINGS };
    let saveTimer = null;

    const close = () => {
        if (!overlay) return;
        overlay.classList.add('hidden');
        document.removeEventListener('keydown', onEsc);
    };

    const open = async () => {
        overlay = mountOverlay();
        overlay.classList.remove('hidden');

        overlay.addEventListener('click', (e) => {
            if (e.target === overlay) close();
        });

        overlay.querySelector('#settingsCloseBtn')?.addEventListener('click', close);

        overlay.querySelectorAll('.settings-nav-item').forEach(btn => {
            btn.addEventListener('click', async () => {
                const sec = btn.getAttribute('data-section');
                if (!sec) return;
                await navigate(sec);
            });
        });

        document.addEventListener('keydown', onEsc);

        await loadAndApply();
        await navigate(activeSection);
    };

    const onEsc = (e) => {
        if (e.key === 'Escape') close();
    };

    const loadAndApply = async () => {
        try {
            const s = await api('/api/users/me/settings');
            currentSettings = normalizeSettings(s);
        } catch (_) {
            currentSettings = normalizeSettings(currentSettings);
        }

        applyUiSettings(currentSettings, applyТема);
    };

    const scheduleSave = (patch) => {
        currentSettings = normalizeSettings({ ...currentSettings, ...(patch || {}) });
        applyUiSettings(currentSettings, applyТема);

        try {
            window.dispatchEvent(new CustomEvent('laberry:settings-changed', { detail: { ...currentSettings } }));
        } catch (_) {}

        if (saveTimer) clearTimeout(saveTimer);
        saveTimer = setTimeout(saveNow, 250);
    };

    const saveNow = async () => {
        try {
            await api('/api/users/me/settings', {
                method: 'PUT',
                body: JSON.stringify(currentSettings),
            });
        } catch (e) {
            console.warn('[SETTINGS] save failed', e);
        }
    };

    const setActiveNav = (section) => {
        overlay?.querySelectorAll('.settings-nav-item').forEach(b => {
            b.classList.toggle('active', b.getAttribute('data-section') === section);
        });
    };

    const setHeader = (title) => {
        const t = overlay?.querySelector('#settingsTitle');
        if (t) t.textContent = title;
    };

    const setBody = (html) => {
        const body = overlay?.querySelector('#settingsBody');
        if (!body) return;
        body.innerHTML = html;
    };

    const showInline = (type, text) => {
        const body = overlay?.querySelector('#settingsBody');
        if (!body) return;

        let box = body.querySelector('.settings-inline');
        if (!box) {
            box = document.createElement('div');
            box.className = 'settings-inline';
            body.prepend(box);
        }

        box.className = `settings-inline ${type}`;
        box.textContent = text;

        setTimeout(() => {
            if (box && box.parentElement) box.remove();
        }, 2500);
    };

    const navigate = async (section) => {
        if (!overlay) return;
        activeSection = section;
        setActiveNav(section);

        if (section === 'logout') {
            await doLogout();
            return;
        }

        if (section === 'account') {
            await renderAccount();
            return;
        }

        if (section === 'privacy') {
            await renderPrivacy();
            return;
        }

        if (section === 'notifications') {
            await renderNotifications();
            return;
        }

        if (section === 'appearance') {
            await renderAppearance();
            return;
        }

        if (section === 'connections') {
            await renderConnections();
            return;
        }

        if (section === 'keybinds') {
            await renderKeybinds();
            return;
        }

        if (section === 'advanced') {
            await renderAdvanced();
            return;
        }

        await renderAccount();
    };

    const doLogout = async () => {
        try {
            await api('/api/auth/logout', { method: 'POST' });
        } catch (_) {}

        localStorage.removeItem('auth_token');
            localStorage.removeItem('refresh_token');
        sessionStorage.clear();
        window.location.href = '/';
    };

    const renderAccount = async () => {
        setHeader('Мой аккаунт');

        const me = getCurrentUser() || (await api('/api/users/me'));
        setCurrentUser(me);

        let status = 'online';
        try {
            const st = await api('/api/users/me/status');
            status = st?.status || 'online';
        } catch (_) {}

        let profile = null;
        try {
            profile = await api('/api/users/me/profile');
        } catch (_) {
            profile = null;
        }

        const avatarFileId = profile?.avatar_file_id || null;
        const avatarLetter = (me?.username || '?').trim().slice(0, 1).toUpperCase() || '?';

        const s = normalizeSettings(currentSettings);

        setBody(`
          <div class="settings-section">
            <div class="settings-card">
              <div class="settings-card-title">Аватар</div>
              <div class="avatar-upload-row">
                <div class="avatar-preview" id="settingsAvatarPreview">
                  <img class="avatar-preview-img ${avatarFileId ? '' : 'hidden'}" id="settingsAvatarImg" alt="avatar" ${avatarFileId ? `src="/api/profile-files/${avatarFileId}/raw"` : ''} />
                  <span class="avatar-preview-letter ${avatarFileId ? 'hidden' : ''}" id="settingsAvatarLetter">${escapeHtml(avatarLetter)}</span>
                </div>

                <div class="avatar-upload-controls">
                  <input id="settingsAvatarFile" type="file" accept="image/png,image/jpeg,image/webp,image/gif" hidden />
                  <div class="avatar-file-row">
                    <label class="btn btn-secondary btn-small" for="settingsAvatarFile">Выберите файл</label>
                    <div class="avatar-file-name" id="settingsAvatarFileName">Файл не выбран</div>
                  </div>
                  <div class="avatar-actions-row">
                    <button class="btn btn-small" id="settingsAvatarUploadBtn" type="button" disabled>Загрузить</button>
                    <div class="avatar-file-hint">PNG/JPG/WEBP/GIF до 12MB.</div>
                  </div>
                </div>
              </div>
            </div>

            <div class="settings-card">
              <div class="settings-kv">
                <div class="settings-k">Имя пользователя</div>
                <div class="settings-v"><span class="mono">${escapeHtml(me?.username || '')}</span></div>
              </div>
            </div>

            <div class="settings-card">
              <h4>Статус</h4>
              <div class="setting-row">
                <div>
                  <div class="setting-title">Текущий статус</div>
                  <div class="setting-desc">online / idle / dnd / invisible</div>
                </div>
                <select class="inp" id="setStatus">
                  <option value="online">online</option>
                  <option value="idle">idle</option>
                  <option value="dnd">dnd</option>
                  <option value="invisible">invisible</option>
                </select>
              </div>
            </div>

            <div class="settings-card">
              <h4>Контакты и ключи</h4>
              <div class="form-row">
                <label>Email</label>
                <input class="inp" id="meEmail" placeholder="email (опционально)" value="${escapeHtml(me?.email || '')}">
              </div>
              <div class="form-row">
                <label>Публичный ключ</label>
                <textarea class="inp" id="mePubKey" rows="3" placeholder="публичный ключ (опционально)">${escapeHtml(me?.public_encryption_key || '')}</textarea>
              </div>
              <div class="form-actions">
                <button class="btn btn-secondary" id="saveProfile" type="button">Сохранить</button>
              </div>
            </div>

            <div class="settings-card">
              <h4>Безопасность</h4>
              <div class="form-row">
                <label>Текущий пароль</label>
                <input class="inp" id="oldPass" type="password" autocomplete="current-password">
              </div>
              <div class="form-row">
                <label>Новый пароль (>= 8 символов)</label>
                <input class="inp" id="newPass" type="password" autocomplete="new-password">
              </div>
              <div class="form-actions">
                <button class="btn" id="changePass" type="button">Изменить пароль</button>
                <button class="btn btn-ghost" id="logoutAll" type="button">Выйти со всех устройств</button>
              </div>
              <div class="muted" style="margin-top:8px; font-size:12px;">Смена пароля сбрасывает все активные сессии.</div>

            <div class="settings-card">
              <h4>Удаление аккаунта</h4>
              <div class="muted" style="margin-top:6px; font-size:12px;">Действие необратимо. Для подтверждения введите ваш username.</div>
              <div class="form-row" style="margin-top:10px;">
                <label>Подтверждение</label>
                <input class="inp" id="deleteMeConfirm" placeholder="${escapeHtml(me?.username || '')}">
              </div>
              <div class="form-actions">
                <button class="btn btn-danger" id="deleteMeBtn" type="button" disabled>Удалить аккаунт</button>
              </div>
            </div>

            </div>
          </div>
        `);

        let selectedFile = null;

        const fileInput = overlay.querySelector('#settingsAvatarFile');
        const fileNameEl = overlay.querySelector('#settingsAvatarFileName');
        const uploadBtn = overlay.querySelector('#settingsAvatarUploadBtn');

        const setFileUi = (f) => {
            selectedFile = f || null;
            if (fileNameEl) fileNameEl.textContent = f ? (f.name || 'Файл выбран') : 'Файл не выбран';
            if (uploadBtn) uploadBtn.disabled = !f;
        };

        fileInput?.addEventListener('change', () => {
            const f = fileInput.files && fileInput.files[0] ? fileInput.files[0] : null;
            if (!f) {
                setFileUi(null);
                return;
            }
            if (f.size > 12 * 1024 * 1024) {
                setFileUi(null);
                showInline('err', 'Файл слишком большой (макс 12MB)');
                return;
            }
            setFileUi(f);
        });

        uploadBtn?.addEventListener('click', async () => {
            if (!selectedFile) return;

            const cropped = await openAvatarCropper(selectedFile, { title: 'Обрезка аватара' });
            if (!cropped) {
                return;
            }

            try {
                const fd = new FormData();
                const outFile = new File([cropped], 'avatar.png', { type: 'image/png' });
                fd.append('file', outFile);

                const up = await api('/api/profile-files', { method: 'POST', body: fd });
                const fileId = up?.id;

                if (!fileId) {
                    showInline('err', 'Не удалось загрузить аватар');
                    return;
                }

                await api('/api/users/me/profile', {
                    method: 'PUT',
                    body: JSON.stringify({ avatar_file_id: fileId }),
                });

                const img = overlay.querySelector('#settingsAvatarImg');
                const letter = overlay.querySelector('#settingsAvatarLetter');
                if (img) {
                    img.src = `/api/profile-files/${fileId}/raw?ts=${Date.now()}`;
                    img.classList.remove('hidden');
                }
                if (letter) letter.classList.add('hidden');

                setFileUi(null);
                try { fileInput.value = ''; } catch (_) {}
                try {
                    window.dispatchEvent(new CustomEvent('laberry:avatar-updated', { detail: { avatar_file_id: fileId } }));
                } catch (_) {}

                showInline('ok', 'Аватар обновлён');
            } catch (e) {
                console.warn('[SETTINGS] avatar upload failed', e);
                showInline('err', 'Не удалось обновить аватар');
            }
        });

        const statusSel = overlay.querySelector('#setStatus');
        if (statusSel) {
            statusSel.value = status;
            statusSel.addEventListener('change', async (e) => {
                const v = e.target?.value || 'online';
                if (typeof updateMyStatus === 'function') {
                    await updateMyStatus(v);
                } else {
                    try {
                        await api('/api/users/me/status', { method: 'PUT', body: JSON.stringify({ status: v }) });
                    } catch (_) {}
                }
                if (typeof applyMyStatusToUI === 'function') applyMyStatusToUI(v);
                showInline('ok', 'Статус обновлён');
            });
        }

        overlay.querySelector('#saveProfile')?.addEventListener('click', async () => {
            const email = (overlay.querySelector('#meEmail')?.value || '').trim();
            const public_encryption_key = (overlay.querySelector('#mePubKey')?.value || '').trim();

            try {
                const updated = await api('/api/users/me', {
                    method: 'PUT',
                    body: JSON.stringify({
                        email: email || null,
                        public_encryption_key: public_encryption_key || null,
                    })
                });
                setCurrentUser(updated);
                showInline('ok', 'Профиль сохранён');
            } catch (e) {
                console.warn('[SETTINGS] profile save failed', e);
                showInline('err', 'Не удалось сохранить профиль');
            }
        });

        overlay.querySelector('#changePass')?.addEventListener('click', async () => {
            const old_password = overlay.querySelector('#oldPass')?.value || '';
            const new_password = overlay.querySelector('#newPass')?.value || '';

            try {
                const res = await api('/api/users/me/password', {
                    method: 'PUT',
                    body: JSON.stringify({ old_password, new_password })
                });
                if (res?.reauth) {
                    showInline('ok', 'Пароль изменён. Выполняю выход...');
                    setTimeout(() => doLogout(), 800);
                } else {
                    showInline('ok', 'Пароль изменён');
                }
            } catch (e) {
                console.warn('[SETTINGS] change password failed', e);
                showInline('err', e?.data?.detail || 'Не удалось изменить пароль');
            }
        });

        overlay.querySelector('#logoutAll')?.addEventListener('click', async () => {
            await doLogout();
        });

        const delInp = overlay.querySelector('#deleteMeConfirm');
        const delBtn = overlay.querySelector('#deleteMeBtn');
        const myU = (me?.username || '').toString();
        const syncDelBtn = () => {
            const v = (delInp?.value || '').toString().trim();
            if (delBtn) delBtn.disabled = !(v && myU && v === myU);
        };

        delInp?.addEventListener('input', syncDelBtn);
        syncDelBtn();

        delBtn?.addEventListener('click', async () => {
            const v = (delInp?.value || '').toString().trim();
            if (!v || v !== myU) {
                showInline('err', 'Введите точный username');
                return;
            }
            if (!confirm('Удалить аккаунт без возможности восстановления?')) return;

            try {
                await api('/api/users/me/delete', {
                    method: 'POST',
                    body: JSON.stringify({ username: v })
                });
                showInline('ok', 'Аккаунт удалён');
                setTimeout(() => doLogout(), 400);
            } catch (e) {
                const code = e?.data?.detail || '';
                if (code === 'username_mismatch') showInline('err', 'Username не совпадает');
                else showInline('err', 'Не удалось удалить аккаунт');
            }
        });
        applyUiSettings(s, applyТема);
    };

    const renderPrivacy = async () => {
        setHeader('Конфиденциальность');

        const s = normalizeSettings(currentSettings);

        setBody(`
          <div class="settings-section">
            <div class="settings-card">
              <h4>Заявки в друзья</h4>
              <div class="setting-row">
                <div>
                  <div class="setting-title">Кто может отправлять заявки</div>
                  <div class="setting-desc">как в Discord: Все / Друзья друзей / Участники сервера / Никто</div>
                </div>
                <select class="inp" id="friendReqMode">
                  <option value="everyone">Все</option>
                  <option value="friends_of_friends">Друзья друзей</option>
                  <option value="server_members">Участники сервера</option>
                  <option value="none">Никто</option>
                </select>
              </div>
            </div>

            <div class="settings-card">
              <h4>Личные сообщения</h4>
              <div class="setting-row">
                <div>
                  <div class="setting-title">Кто может писать в ЛС</div>
                  <div class="setting-desc">ограничение применяется при создании приватных чатов</div>
                </div>
                <select class="inp" id="dmMode">
                  <option value="friends_only">Только друзья</option>
                  <option value="friends_and_server">Друзья + участники сервера</option>
                  <option value="everyone">Все</option>
                </select>
              </div>
            </div>
          </div>
        `);

        const fr = overlay.querySelector('#friendReqMode');
        if (fr) {
            fr.value = s.friend_requests;
            fr.addEventListener('change', (e) => {
                scheduleSave({ friend_requests: e.target?.value || 'everyone' });
                showInline('ok', 'Сохранено');
            });
        }

        const dm = overlay.querySelector('#dmMode');
        if (dm) {
            dm.value = s.dms;
            dm.addEventListener('change', (e) => {
                scheduleSave({ dms: e.target?.value || 'friends_and_server' });
                showInline('ok', 'Сохранено');
            });
        }
    };

    const renderNotifications = async () => {
        setHeader('Уведомления', 'Хранится на сервере');

        const s = normalizeSettings(currentSettings);

        setBody(`
          <div class="settings-section">
            <div class="settings-card">
              <h4>Общие</h4>
              <div class="setting-row">
                <div>
                  <div class="setting-title">Уведомления на рабочем столе</div>
                </div>
                <label class="switch">
                  <input type="checkbox" id="notifyDesktop" ${s.notify_desktop ? 'checked' : ''}>
                  <span class="slider"></span>
                </label>
              </div>
              <div class="setting-row">
                <div>
                  <div class="setting-title">Звуки</div>
                </div>
                <label class="switch">
                  <input type="checkbox" id="notifyЗвукs" ${s.notify_sounds ? 'checked' : ''}>
                  <span class="slider"></span>
                </label>
              </div>
            </div>

            <div class="settings-card">
              <h4>События</h4>
              <div class="setting-row">
                <div>
                  <div class="setting-title">Личные сообщения</div>
                </div>
                <label class="switch">
                  <input type="checkbox" id="notifyDMs" ${s.notify_dms ? 'checked' : ''}>
                  <span class="slider"></span>
                </label>
              </div>
              <div class="setting-row">
                <div>
                  <div class="setting-title">Упоминания</div>
                </div>
                <label class="switch">
                  <input type="checkbox" id="notifyMentions" ${s.notify_mentions ? 'checked' : ''}>
                  <span class="slider"></span>
                </label>
              </div>
            </div>
          </div>
        `);

        const bindToggle = (id, key) => {
            const el = overlay.querySelector(id);
            if (!el) return;
            el.addEventListener('change', () => {
                scheduleSave({ [key]: !!el.checked });
                showInline('ok', 'Сохранено');
            });
        };

        const desk = overlay.querySelector('#notifyDesktop');
        if (desk) {
            desk.addEventListener('change', async () => {
                const want = !!desk.checked;

                if (want) {
                    if (typeof Notification === 'undefined') {
                        desk.checked = false;
                        scheduleSave({ notify_desktop: false });
                        showInline('err', 'Браузер не поддерживает уведомления');
                        return;
                    }

                    try {
                        if (Notification.permission === 'default') {
                            const p = await Notification.requestPermission();
                            if (p !== 'granted') {
                                desk.checked = false;
                                scheduleSave({ notify_desktop: false });
                                showInline('err', 'Разрешение на уведомления не выдано');
                                return;
                            }
                        }

                        if (Notification.permission !== 'granted') {
                            desk.checked = false;
                            scheduleSave({ notify_desktop: false });
                            showInline('err', 'Уведомления заблокированы в браузере');
                            return;
                        }
                    } catch (_) {}
                }

                scheduleSave({ notify_desktop: want });
                showInline('ok', 'Сохранено');
            });
        }
        bindToggle('#notifyЗвукs', 'notify_sounds');
        bindToggle('#notifyDMs', 'notify_dms');
        bindToggle('#notifyMentions', 'notify_mentions');
    };

    const renderAppearance = async () => {
        setHeader('Внешний вид', 'Тема, плотность, таймстемпы');

        const s = normalizeSettings(currentSettings);

        setBody(`
          <div class="settings-section">
            <div class="settings-card">
              <h4>Тема</h4>
              <div class="setting-row">
                <div>
                  <div class="setting-title">Цветовая схема</div>
                </div>
                <select class="inp" id="themeSel">
                  <option value="dark">Тёмная</option>
                  <option value="light">Светлая</option>
                </select>
              </div>
              <div class="setting-row">
                <div>
                  <div class="setting-title">Показывать статус в шапке</div>
                </div>
                <label class="switch">
                  <input type="checkbox" id="headerStatus" ${s.show_header_status ? 'checked' : ''}>
                  <span class="slider"></span>
                </label>
              </div>
            
            <div class="settings-card">
              <h4>Интерфейс</h4>
              <div class="setting-row">
                <div>
                  <div class="setting-title">Масштаб интерфейса</div>
                  <div class="setting-desc">80% — больше помещается, 130% — крупнее</div>
                </div>
                <div class="range-wrap">
                  <input type="range" id="fontScale" min="80" max="130" step="5" value="${Math.round((s.font_scale || 1) * 100)}">
                  <div class="range-val" id="fontScaleVal">${Math.round((s.font_scale || 1) * 100)}%</div>
                </div>
              </div>
            </div>

</div>

            <div class="settings-card">
              <h4>Сообщения</h4>
              <div class="setting-row">
                <div>
                  <div class="setting-title">Компактный режим</div>
                </div>
                <label class="switch">
                  <input type="checkbox" id="compactMode" ${s.compact_mode ? 'checked' : ''}>
                  <span class="slider"></span>
                </label>
              </div>
              <div class="setting-row">
                <div>
                  <div class="setting-title">Показывать время</div>
                </div>
                <label class="switch">
                  <input type="checkbox" id="showTime" ${s.show_timestamps ? 'checked' : ''}>
                  <span class="slider"></span>
                </label>
              </div>
            </div>
          </div>
        `);

        const themeSel = overlay.querySelector('#themeSel');
        if (themeSel) {
            themeSel.value = s.theme;
            themeSel.addEventListener('change', () => {
                scheduleSave({ theme: themeSel.value || 'dark' });
                showInline('ok', 'Сохранено');
            });
        }

        const bindToggle = (id, key) => {
            const el = overlay.querySelector(id);
            if (!el) return;
            el.addEventListener('change', () => {
                scheduleSave({ [key]: !!el.checked });
                showInline('ok', 'Сохранено');
            });
        };

        bindToggle('#headerStatus', 'show_header_status');
        bindToggle('#compactMode', 'compact_mode');
        bindToggle('#showTime', 'show_timestamps');

        const fs = overlay.querySelector('#fontScale');
        const fsVal = overlay.querySelector('#fontScaleVal');
        if (fs) {
            fs.addEventListener('input', () => {
                const pct = Number(fs.value || 100);
                if (fsVal) fsVal.textContent = `${pct}%`;
            });
            fs.addEventListener('change', () => {
                const pct = Number(fs.value || 100);
                if (fsVal) fsVal.textContent = `${pct}%`;
                scheduleSave({ font_scale: pct / 100 });
                showInline('ok', 'Сохранено');
            });
        }
    };



    const iconForKind = (kind) => {
        const k = (kind || 'other').toString().toLowerCase();
        if (k === 'discord') return '🟣';
        if (k === 'telegram') return '🔵';
        if (k === 'github') return '⚫';
        if (k === 'youtube') return '🔴';
        if (k === 'twitch') return '🟪';
        if (k === 'website') return '🌐';
        return '🔗';
    };

    const renderConnections = async () => {
        setHeader('Интеграции', 'Показываются в профиле');

        const s = normalizeSettings(currentSettings);
        const items = (s.connections || []).map((c, idx) => {
            const label = c.label ? escapeHtml(c.label) : escapeHtml(c.url);
            const url = escapeHtml(c.url);
            const kind = escapeHtml(c.kind || 'other');

            return `
              <div class="conn-item" data-idx="${idx}">
                <div class="conn-kind">${iconForKind(kind)}</div>
                <div class="conn-main">
                  <div class="conn-label">${label}</div>
                  <a class="conn-url" href="${url}" target="_blank" rel="noopener noreferrer">${url}</a>
                </div>
                <button class="btn btn-ghost conn-del" type="button" data-idx="${idx}">Удалить</button>
              </div>
            `;
        }).join('');

        setBody(`
          <div class="settings-section">
            <div class="settings-card">
              <h4>Подключённые аккаунты</h4>
              <div class="muted" style="margin-bottom:10px; font-size:12px;">Тут пока только ссылки (как в Discord Connections).</div>
              <div class="conn-list">
                ${items || '<div class="muted">Пока нет интеграций</div>'}
              </div>
            </div>

            <div class="settings-card">
              <h4>Добавить</h4>
              <div class="setting-row">
                <div>
                  <div class="setting-title">Сервис</div>
                </div>
                <select class="inp" id="connKind">
                  <option value="discord">Discord</option>
                  <option value="telegram">Telegram</option>
                  <option value="github">GitHub</option>
                  <option value="youtube">YouTube</option>
                  <option value="twitch">Twitch</option>
                  <option value="website">Website</option>
                  <option value="other">Other</option>
                </select>
              </div>
              <div class="form-row">
                <label>URL</label>
                <input class="inp" id="connUrl" placeholder="https://...">
              </div>
              <div class="form-row">
                <label>Подпись (опционально)</label>
                <input class="inp" id="connLabel" placeholder="например: мой GitHub">
              </div>
              <div class="form-actions">
                <button class="btn" id="connAdd" type="button">Добавить</button>
              </div>
            </div>
          </div>
        `);

        overlay.querySelectorAll('.conn-del').forEach((btn) => {
            btn.addEventListener('click', () => {
                const idx = Number(btn.getAttribute('data-idx'));
                if (!Number.isFinite(idx)) return;

                const next = (normalizeSettings(currentSettings).connections || []).slice();
                next.splice(idx, 1);

                scheduleSave({ connections: next });
                showInline('ok', 'Удалено');
                renderConnections();
            });
        });

        overlay.querySelector('#connAdd')?.addEventListener('click', () => {
            const kind = (overlay.querySelector('#connKind')?.value || 'other').toString();
            let url = (overlay.querySelector('#connUrl')?.value || '').toString().trim();
            const label = (overlay.querySelector('#connLabel')?.value || '').toString().trim();

            if (!url) {
                showInline('err', 'Укажи URL');
                return;
            }
            if (!/^https?:\/\//i.test(url)) url = `https://${url}`;

            const next = (normalizeSettings(currentSettings).connections || []).slice();
            next.push({ kind, url, label: label || undefined });

            scheduleSave({ connections: next });
            showInline('ok', 'Добавлено');
            renderConnections();
        });
    };
    const renderKeybinds = async () => {
        setHeader('Клавиши', 'Заготовка под голос/рацию');

        setBody(`
          <div class="settings-section">
            <div class="settings-card">
              <h4>Push-to-talk</h4>
              <div class="muted">Пока не используется в чате, но настройки сохраняются.</div>
            </div>
          </div>
        `);
    };

    const renderAdvanced = async () => {
        setHeader('Расширенные', 'Разработчик');

        const s = normalizeSettings(currentSettings);

        setBody(`
          <div class="settings-section">
            <div class="settings-card">
              <div class="setting-row">
                <div>
                  <div class="setting-title">Режим разработчика</div>
                  <div class="setting-desc">для логов/UI</div>
                </div>
                <label class="switch">
                  <input type="checkbox" id="devMode" ${s.developer_mode ? 'checked' : ''}>
                  <span class="slider"></span>
                </label>
              </div>
            </div>
          </div>
        `);

        const dev = overlay.querySelector('#devMode');
        if (dev) {
            dev.addEventListener('change', () => {
                scheduleSave({ developer_mode: !!dev.checked });
                showInline('ok', 'Сохранено');
            });
        }
    };

    return {
        open,
        close,
        loadAndApply,
        getSettings: () => ({ ...currentSettings }),
    };
}

