(() => {
  const STORAGE_PANEL = 'lb-admin-center-active-panel';

  const qs = (s, root = document) => root.querySelector(s);
  const qsa = (s, root = document) => Array.from(root.querySelectorAll(s));

  function showPanel(name) {
    const views = qsa('[data-panel-view]');
    const buttons = qsa('[data-center-switch]');
    const titleEl = qs('[data-center-stage-title]');
    const subEl = qs('[data-center-stage-sub]');
    let found = false;

    views.forEach((view) => {
      const active = view.dataset.panelView === name;
      view.classList.toggle('is-active', active);
      if (active) {
        found = true;
        if (titleEl) titleEl.textContent = view.dataset.stageTitle || 'Панель';
        if (subEl) subEl.textContent = view.dataset.stageSub || '';
      }
    });

    buttons.forEach((btn) => {
      btn.classList.toggle('active', btn.dataset.centerSwitch === name);
    });

    if (found) {
      localStorage.setItem(STORAGE_PANEL, name);
    }
  }

  function applyTextFilter(kind, value) {
    const norm = (value || '').trim().toLowerCase();
    qsa(`[data-filter-item='${kind}']`).forEach((item) => {
      const hay = (item.dataset.filter || '').toLowerCase();
      item.style.display = !norm || hay.includes(norm) ? '' : 'none';
    });
  }

  function setupPersistedInputs() {
    qsa('[data-persist-key]').forEach((input) => {
      const key = input.dataset.persistKey;
      const saved = localStorage.getItem(key);
      if (saved !== null && !input.value) input.value = saved;

      const handler = () => {
        localStorage.setItem(key, input.value || '');
        if (input.dataset.filterInput) applyTextFilter(input.dataset.filterInput, input.value || '');
      };

      input.addEventListener('input', handler);
      handler();
    });
  }

  function setupClearButtons() {
    qsa('[data-clear-filter]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const kind = btn.dataset.clearFilter;
        const input = qs(`[data-filter-input='${kind}']`);
        if (!input) return;
        input.value = '';
        input.dispatchEvent(new Event('input', { bubbles: true }));
      });
    });
  }

  function setupPanelButtons() {
    qsa('[data-center-switch]').forEach((btn) => {
      btn.addEventListener('click', () => showPanel(btn.dataset.centerSwitch));
    });

    const params = new URLSearchParams(window.location.search);
    const fromUrl = params.get('view');
    const saved = fromUrl || localStorage.getItem(STORAGE_PANEL) || 'overview';
    showPanel(saved);
  }

  document.addEventListener('DOMContentLoaded', () => {
    setupPanelButtons();
    setupPersistedInputs();
    setupClearButtons();
  });
})();
