(() => {
  const revealItems = Array.from(document.querySelectorAll('[data-reveal]'));

  function initReveal() {
    if (!revealItems.length) return;

    if (!('IntersectionObserver' in window)) {
      revealItems.forEach((item) => item.classList.add('is-visible'));
      return;
    }

    const observer = new IntersectionObserver((entries) => {
      entries.forEach((entry) => {
        if (!entry.isIntersecting) return;
        entry.target.classList.add('is-visible');
        observer.unobserve(entry.target);
      });
    }, { threshold: 0.18 });

    revealItems.forEach((item) => observer.observe(item));
  }

  function byPlatform(items, platform) {
    return (Array.isArray(items) ? items : []).find((item) => item?.platform === platform) || null;
  }

  function setDownload(platform, item) {
    const link = document.querySelector(`[data-download-link="${platform}"]`);
    const meta = document.querySelector(`[data-download-meta="${platform}"]`);
    if (!link || !meta) return;

    const available = !!item?.available && !!item?.download_url;
    const version = String(item?.version || '').trim();
    const size = Number(item?.file_size || 0);
    const sizeText = size > 0 ? ` • ${formatBytes(size)}` : '';

    link.classList.toggle('is-loading', false);
    link.classList.toggle('is-available', available);
    link.setAttribute('aria-disabled', available ? 'false' : 'true');

    if (available) {
      link.href = item.download_url;
      meta.textContent = `${version ? `Версия ${version}` : 'Версия доступна'}${sizeText}`;
    } else {
      link.href = '#';
      meta.textContent = 'Сборка пока не загружена';
    }
  }

  function formatBytes(bytes) {
    if (!Number.isFinite(bytes) || bytes <= 0) return '';
    const units = ['Б', 'КБ', 'МБ', 'ГБ'];
    let value = bytes;
    let idx = 0;
    while (value >= 1024 && idx < units.length - 1) {
      value /= 1024;
      idx += 1;
    }
    return `${value.toFixed(value >= 10 || idx === 0 ? 0 : 1)} ${units[idx]}`;
  }

  async function loadDownloads() {
    try {
      const res = await fetch('/api/downloads/', { cache: 'no-store' });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const items = await res.json();
      setDownload('android', byPlatform(items, 'android'));
      setDownload('pc', byPlatform(items, 'pc'));
    } catch (_) {
      setDownload('android', null);
      setDownload('pc', null);
    }
  }

  function wireDownloadGuards() {
    document.querySelectorAll('[data-download-link]').forEach((link) => {
      link.addEventListener('click', (event) => {
        if (link.getAttribute('aria-disabled') === 'true') {
          event.preventDefault();
        }
      });
    });
  }

  function setOnline(count) {
    const safeCount = Number.isFinite(count) ? Math.max(0, count) : 0;
    document.querySelectorAll('[data-online-count]').forEach((el) => {
      el.textContent = String(safeCount);
    });
  }

  async function loadOnline() {
    try {
      const res = await fetch('/api/presence/stats', { cache: 'no-store' });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json();
      setOnline(Number(data?.online_count || 0));
    } catch (_) {
      setOnline(0);
    }
  }

  function initLaunchTransition() {
    const link = document.querySelector('[data-launch-link]');
    if (!link) return;

    link.addEventListener('click', (event) => {
      if (event.defaultPrevented) return;
      if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;

      const href = link.getAttribute('href') || '';
      if (!href || href === '#') return;

      event.preventDefault();
      if (document.body.classList.contains('is-launching')) return;

      document.body.classList.add('is-launching');

      const reduced = window.matchMedia?.('(prefers-reduced-motion: reduce)')?.matches;
      window.setTimeout(() => {
        window.location.href = href;
      }, reduced ? 80 : 720);
    });
  }

  initReveal();
  initLaunchTransition();
  wireDownloadGuards();
  loadDownloads();
  loadOnline();
  setInterval(loadOnline, 60_000);
})();
