(() => {
  const qs = (s, root = document) => root.querySelector(s);
  const qsa = (s, root = document) => Array.from(root.querySelectorAll(s));

  function getDetailPane() {
    return qs('[data-admin-user-detail]');
  }

  function getActiveRow() {
    return qs('[data-admin-user-row].active');
  }

  async function fetchHtml(url, opts = {}) {
    const res = await fetch(url, {
      credentials: 'same-origin',
      headers: { 'X-Requested-With': 'fetch' },
      ...opts,
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return await res.text();
  }

  function setActiveRow(row) {
    qsa('[data-admin-user-row]').forEach((el) => el.classList.toggle('active', el === row));
  }


  function notify(message, kind = 'error') {
    let box = qs('#adminUserToast');
    if (!box) {
      box = document.createElement('div');
      box.id = 'adminUserToast';
      box.className = 'admin-user-toast';
      document.body.appendChild(box);
    }
    box.textContent = message;
    box.dataset.kind = kind;
    box.hidden = false;
    clearTimeout(box._hideTimer);
    box._hideTimer = setTimeout(() => { box.hidden = true; }, 2600);
  }

  async function readErrorText(res) {
    try {
      const text = await res.text();
      return text ? text.slice(0, 220) : `HTTP ${res.status}`;
    } catch (_) {
      return `HTTP ${res.status}`;
    }
  }

  function updateRowStatusFromCard(card) {
    if (!card) return;
    const id = card.getAttribute('data-user-id');
    const banned = card.getAttribute('data-user-banned') === '1';
    const online = card.getAttribute('data-user-online') === '1';
    const row = qs(`[data-admin-user-row][data-user-id='${CSS.escape(id)}']`);
    if (!row) return;
    const pill = qs('.admin-user-pill', row);
    if (!pill) return;
    pill.className = `admin-user-pill ${banned ? 'banned' : (online ? 'online' : 'offline')}`;
    pill.textContent = banned ? 'Бан' : (online ? 'Онлайн' : 'Оффлайн');
  }

  async function loadUserCard(rowOrUrl, push = true) {
    const pane = getDetailPane();
    if (!pane) return;
    const row = typeof rowOrUrl === 'string' ? null : rowOrUrl;
    const url = row ? row.getAttribute('data-card-url') : rowOrUrl;
    if (!url) return;
    pane.setAttribute('aria-busy', 'true');
    const html = await fetchHtml(url);
    pane.innerHTML = html;
    pane.removeAttribute('aria-busy');
    const card = qs('[data-admin-user-detail-card]', pane);
    updateRowStatusFromCard(card);
    if (row) {
      setActiveRow(row);
      const href = row.getAttribute('href');
      if (push && href) history.replaceState(null, '', href);
    }
  }

  function ensureModal() {
    let backdrop = qs('#adminUserModalBackdrop');
    if (backdrop) return backdrop;
    backdrop = document.createElement('div');
    backdrop.id = 'adminUserModalBackdrop';
    backdrop.className = 'admin-modal-backdrop';
    backdrop.hidden = true;
    backdrop.innerHTML = `<div class="admin-modal-window" role="dialog" aria-modal="true" data-admin-modal-content></div>`;
    backdrop.addEventListener('click', (ev) => {
      if (ev.target === backdrop || ev.target.hasAttribute('data-admin-modal-close')) {
        closeModal();
      }
    });
    document.addEventListener('keydown', (ev) => {
      if (ev.key === 'Escape') closeModal();
    });
    document.body.appendChild(backdrop);
    return backdrop;
  }

  function closeModal() {
    const backdrop = qs('#adminUserModalBackdrop');
    if (backdrop) backdrop.hidden = true;
  }

  async function openDetails(url) {
    const backdrop = ensureModal();
    const content = qs('[data-admin-modal-content]', backdrop);
    if (!content) return;
    content.innerHTML = '<div class="admin-user-emptyline">Загрузка...</div>';
    backdrop.hidden = false;
    content.innerHTML = await fetchHtml(url);
  }

  function setup() {
    document.addEventListener('click', async (ev) => {
      const detailsBtn = ev.target.closest('[data-admin-user-details]');
      if (detailsBtn) {
        ev.preventDefault();
        ev.stopPropagation();
        const url = detailsBtn.getAttribute('data-details-url');
        if (url) {
          try { await openDetails(url); } catch (e) { notify('Не удалось открыть детали'); }
        }
        return;
      }

      const row = ev.target.closest('[data-admin-user-row]');
      if (row) {
        ev.preventDefault();
        try { await loadUserCard(row); } catch (e) { window.location.href = row.href; }
        return;
      }
    });

    document.addEventListener('submit', async (ev) => {
      const form = ev.target.closest('[data-ajax-user-action], [data-ajax-report-status]');
      if (!form) return;
      ev.preventDefault();
      if (form.hasAttribute('data-danger-action') && !confirm('Точно выполнить опасное действие?')) return;
      try {
        const payload = new URLSearchParams();
        new FormData(form).forEach((value, key) => payload.append(key, value));
        const res = await fetch(form.action, {
          method: 'POST',
          credentials: 'same-origin',
          body: payload,
          headers: {
            'X-Requested-With': 'fetch',
            'Content-Type': 'application/x-www-form-urlencoded;charset=UTF-8',
          },
          redirect: 'manual',
        });
        const isRedirect = res.type === 'opaqueredirect' || (res.status >= 300 && res.status < 400);
        if (!res.ok && !isRedirect) throw new Error(await readErrorText(res));
        notify('Готово', 'ok');
        const active = getActiveRow();
        if (active) {
          try {
            await loadUserCard(active, false);
          } catch (_) {
            active.remove();
            const pane = getDetailPane();
            if (pane) pane.innerHTML = '<div class="admin-user-emptyline">Действие выполнено. Карточка больше недоступна.</div>';
          }
        }
      } catch (e) {
        notify(`Действие не выполнено: ${e.message || e}`);
      }
    });
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', setup);
  else setup();
})();
