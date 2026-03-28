import { api as defaultApi } from "./api.js?v=10";
import { openAvatarCropper } from "./avatar-cropper.js?v=7";

export function initProfileModal({ api, getMe } = {}) {
  const apiFn = typeof api === 'function' ? api : defaultApi;
  const getMeFn = typeof getMe === 'function' ? getMe : () => null;

  function avatarRawUrl(fileId) {
    const id = Number(fileId);
    if (!Number.isFinite(id) || id <= 0) return null;
    return `/api/profile-files/${id}/raw`;
  }

  function avatarInnerHtml(fileId, usernameFallback) {
    const url = avatarRawUrl(fileId);
    if (url) {
      const alt = escapeAttr(usernameFallback || '');
      return `<img class="profile-avatar-img" style="width:100%;height:100%;object-fit:cover;display:block;" src="${url}" alt="${alt}">`;
    }
    const letter = String(usernameFallback || '?').trim().charAt(0).toUpperCase() || '?';
    return escapeHtml(letter);
  }

  function isAnimatedGifFile(file) {
    const type = (file?.type || '').toString().toLowerCase();
    const name = (file?.name || '').toString().toLowerCase();
    return type === 'image/gif' || name.endsWith('.gif');
  }

  let overlay = document.getElementById('profileOverlay');
  if (!overlay) {
    overlay = document.createElement('div');
    overlay.id = 'profileOverlay';
    overlay.className = 'modal-overlay';
    overlay.hidden = true;
    overlay.innerHTML = `
      <div class="modal profile-modal" role="dialog" aria-modal="true">
        <div class="modal-header">
          <div class="modal-title" id="profileTitle">Профиль</div>
          <button type="button" class="icon-btn" id="profileCloseBtn" title="Закрыть">✕</button>
        </div>
        <div class="modal-body" id="profileBody"></div>
      </div>
    `;
    document.body.appendChild(overlay);
  }

  const closeBtn = overlay.querySelector('#profileCloseBtn');
  const bodyEl = overlay.querySelector('#profileBody');
  const titleEl = overlay.querySelector('#profileTitle');

  function close() {
    overlay.hidden = true;
    overlay.classList.remove('show');
    document.body.classList.remove('modal-open');
  }

  function open() {
    overlay.hidden = false;
    overlay.classList.add('show');
    document.body.classList.add('modal-open');
  }

  overlay.addEventListener('click', (e) => {
    if (e.target === overlay) close();
  });
  closeBtn?.addEventListener('click', close);
  document.addEventListener('keydown', (e) => {
    if (!overlay.hidden && e.key === 'Escape') close();
  });

  async function renderProfile(userId) {
    const uid = Number(userId);
    if (!Number.isFinite(uid) || uid <= 0) return;

    const me = getMeFn() || {};
    const meId = Number(me?.id);
    const isMe = Number.isFinite(meId) && meId === uid;

    bodyEl.innerHTML = `<div class="muted" style="padding:10px;">Загрузка...</div>`;

    try {
      const [user, profile] = await Promise.all([
        apiFn(`/api/users/${uid}`),
        apiFn(isMe ? `/api/users/me/profile` : `/api/users/${uid}/profile`)
      ]);

      const username = (user?.username || '').toString() || `User#${uid}`;
      const nickname = (user?.nickname || '').toString().trim();
      const display = nickname || username;

      const about = (profile?.about || '').toString().trim();
      const statusText = (profile?.status_text || '').toString().trim();
      const avatarFileId = profile?.avatar_file_id;

      titleEl.textContent = display;

      bodyEl.innerHTML = `
        <div class="profile-shell">
          <div class="profile-head">
            <div class="profile-avatar-wrap">
              <div class="profile-avatar" id="profileAvatar">${avatarInnerHtml(avatarFileId, display)}</div>
              <div class="profile-status-pill">${statusText ? escapeHtml(statusText) : 'Без статуса'}</div>
            </div>
            <div class="profile-meta">
              <div class="profile-name-row">
                <div class="profile-name">${escapeHtml(display)}</div>
                ${isMe ? '<span class="profile-chip">Это вы</span>' : ''}
              </div>
              <div class="profile-username">@${escapeHtml(username)}</div>
              <div class="profile-id">ID ${escapeHtml(uid)}</div>
            </div>
          </div>

          <div class="profile-grid">
            ${isMe ? `
              <div class="profile-card">
                <div class="profile-label">Аватар</div>
                <div class="profile-avatar-row">
                  <input class="file-input" id="profileAvatarFile" type="file" accept="image/*" />
                  <button type="button" class="btn" id="profileAvatarPickBtn">Изменить</button>
                  <span class="muted" id="profileAvatarHint" style="display:none;">Готово</span>
                </div>
                <div class="muted" style="margin-top:8px; font-size:12px;">PNG/JPG/WEBP/GIF до 12MB. GIF загружается без обрезки, чтобы сохранить анимацию.</div>
              </div>
            ` : ''}

            <div class="profile-card">
              <div class="profile-label">Статус</div>
              ${isMe ? `
                <input class="inp" id="profileStatusInp" placeholder="Например: занят" value="${escapeAttr(statusText)}" />
              ` : `
                <div class="profile-text${statusText ? '' : ' empty'}">${statusText ? escapeHtml(statusText) : 'Пусто'}</div>
              `}
            </div>

            <div class="profile-card">
              <div class="profile-label">О себе</div>
              ${isMe ? `
                <textarea class="inp" id="profileAboutInp" rows="4" placeholder="Коротко о себе">${escapeHtml(about)}</textarea>
              ` : `
                <div class="profile-text${about ? '' : ' empty'}">${about ? escapeHtml(about) : 'Пользователь ничего не рассказал о себе'}</div>
              `}
            </div>
          </div>

          ${isMe ? `
            <div class="profile-actions">
              <div class="muted" id="profileSaveHint" style="display:none;">Сохранено</div>
              <button type="button" class="btn btn-primary" id="profileSaveBtn">Сохранить</button>
            </div>
          ` : ''}
        </div>
      `;

      if (isMe) {
        const avatarFile = bodyEl.querySelector('#profileAvatarFile');
        const pickBtn = bodyEl.querySelector('#profileAvatarPickBtn');
        const avatarHint = bodyEl.querySelector('#profileAvatarHint');
        const avatarBox = bodyEl.querySelector('#profileAvatar');

        const uploadAvatarFile = async (fileOrBlob, fallbackName = 'avatar.png', fallbackType = 'image/png') => {
          if (!fileOrBlob) return;
          const fd = new FormData();
          const safeName = (fileOrBlob?.name || fallbackName || 'avatar.png').toString();
          const safeType = (fileOrBlob?.type || fallbackType || 'image/png').toString();
          const payload = fileOrBlob instanceof File ? fileOrBlob : new File([fileOrBlob], safeName, { type: safeType });
          fd.append('file', payload, safeName);

          const uploaded = await apiFn('/api/profile-files', { method: 'POST', body: fd });
          const fileId = Number(uploaded?.id);
          if (!Number.isFinite(fileId) || fileId <= 0) throw new Error('bad profile file id');

          const updated = await apiFn('/api/users/me/profile', {
            method: 'PUT',
            body: JSON.stringify({ avatar_file_id: fileId }),
          });

          const newId = Number(updated?.avatar_file_id) || fileId;
          if (avatarBox) {
            avatarBox.innerHTML = avatarInnerHtml(newId, display);
            const img = avatarBox.querySelector('img');
            if (img) img.src = avatarRawUrl(newId) + `?t=${Date.now()}`;
          }

          try {
            window.dispatchEvent(new CustomEvent('laberry:avatar-updated', { detail: { avatar_file_id: newId } }));
          } catch (_) {}

          if (avatarHint) {
            avatarHint.style.display = 'inline';
            setTimeout(() => { try { avatarHint.style.display = 'none'; } catch (_) {} }, 1200);
          }
        };

        pickBtn?.addEventListener('click', () => avatarFile?.click());
        avatarBox?.addEventListener('click', () => avatarFile?.click());

        avatarFile?.addEventListener('change', async () => {
          const f = avatarFile?.files?.[0];
          if (!f) return;
          if (pickBtn) pickBtn.disabled = true;
          try {
            if (isAnimatedGifFile(f)) {
              await uploadAvatarFile(f, f.name || 'avatar.gif', f.type || 'image/gif');
              return;
            }
            const blob = await openAvatarCropper(f, { title: 'Настройка аватара' });
            if (!blob) return;
            await uploadAvatarFile(blob, 'avatar.png', 'image/png');
          } catch (e) {
            console.warn('[PROFILE] avatar upload failed', e);
            alert('Не удалось обновить аватар');
          } finally {
            try { avatarFile.value = ''; } catch (_) {}
            if (pickBtn) pickBtn.disabled = false;
          }
        });
        const saveBtn = bodyEl.querySelector('#profileSaveBtn');
        const hint = bodyEl.querySelector('#profileSaveHint');
        const statusInp = bodyEl.querySelector('#profileStatusInp');
        const aboutInp = bodyEl.querySelector('#profileAboutInp');

        saveBtn?.addEventListener('click', async () => {
          try {
            saveBtn.disabled = true;
            await apiFn('/api/users/me/profile', {
              method: 'PUT',
              body: JSON.stringify({
                about: (aboutInp?.value || '').toString(),
                status_text: (statusInp?.value || '').toString(),
              }),
            });
            if (hint) {
              hint.style.display = 'block';
              setTimeout(() => { try { hint.style.display = 'none'; } catch (_) {} }, 1200);
            }
          } catch (e) {
            alert('Не удалось сохранить профиль');
          } finally {
            saveBtn.disabled = false;
          }
        });
      }

    } catch (e) {
      console.warn('[PROFILE] load failed', e);
      bodyEl.innerHTML = `<div class="error" style="margin:10px;">Не удалось загрузить профиль</div>`;
    }
  }

  window.addEventListener('laberry:profile-open', (ev) => {
    const uid = ev?.detail?.userId;
    renderProfile(uid);
    open();
  });

  // self open helper
  window.lbOpenMyProfile = () => {
    const me = getMeFn() || {};
    if (me?.id) {
      renderProfile(me.id);
      open();
    }
  };
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/\"/g, "&quot;")
    .replace(/'/g, "&#039;");
}

function escapeAttr(s) {
  return escapeHtml(s).replace(/\n/g, ' ');
}
