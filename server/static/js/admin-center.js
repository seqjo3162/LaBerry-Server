(() => {
  const STORAGE_PANEL = 'lb-admin-center-active-panel';
  const STORAGE_CHAT = 'lb-admin-center-active-chat';

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
      if (saved !== null && !input.value) {
        input.value = saved;
      }

      const handler = () => {
        localStorage.setItem(key, input.value || '');
        if (input.dataset.filterInput) {
          applyTextFilter(input.dataset.filterInput, input.value || '');
        }
        if (input.hasAttribute('data-chat-search')) {
          applyChatSearch(input.value || '');
        }
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

    qsa('[data-clear-chat-search]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const input = qs('[data-chat-search]');
        if (!input) return;
        input.value = '';
        input.dispatchEvent(new Event('input', { bubbles: true }));
      });
    });
  }

  function applyChatSearch(value) {
    const norm = (value || '').trim().toLowerCase();
    qsa('[data-chat-select]').forEach((btn) => {
      const text = btn.textContent.toLowerCase();
      btn.style.display = !norm || text.includes(norm) ? '' : 'none';
    });
  }

  function selectChat(chatId) {
    qsa('[data-chat-select]').forEach((btn) => {
      btn.classList.toggle('is-active', btn.dataset.chatSelect === String(chatId));
    });
    qsa('[data-chat-feed]').forEach((item) => {
      item.style.display = !chatId || item.dataset.chatFeed === String(chatId) ? '' : 'none';
    });
    if (chatId) {
      localStorage.setItem(STORAGE_CHAT, String(chatId));
    }
  }

  function setupMessenger() {
    const chatButtons = qsa('[data-chat-select]');
    if (!chatButtons.length) return;

    chatButtons.forEach((btn) => {
      btn.addEventListener('click', () => selectChat(btn.dataset.chatSelect));
    });

    const saved = localStorage.getItem(STORAGE_CHAT);
    const firstVisible = chatButtons.find((b) => b.style.display !== 'none');
    const target = chatButtons.find((b) => b.dataset.chatSelect === saved) || firstVisible;
    if (target) {
      selectChat(target.dataset.chatSelect);
    }
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

  function escapeHtml(s) {
    return String(s ?? '')
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
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

  function applyInlineMarksSafe(escaped) {
    let html = (escaped ?? '').toString();
    if (html.length > 1400) return html;

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
    if (src.length > 1400) return escapeHtml(src);

    const tokenRe = /`([^`\n]{1,260})`|\[([^\]\n]{1,120})\]\((https?:\/\/[^\s)<>]{1,500})\)|(https?:\/\/[^\s<]{1,650})/gi;
    let out = '';
    let pos = 0;
    let guard = 0;

    for (const m of src.matchAll(tokenRe)) {
      guard += 1;
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
      || /^\s{0,3}#{1,6}\s+/.test(line)
      || /^\s{0,3}([-*_])\s*\1\s*\1[\s\1]*$/.test(line)
      || /^\s{0,3}>\s?/.test(line)
      || /^\s{0,3}(?:[-*+]\s+|\d{1,4}[.)]\s+)/.test(line);
  }

  function shouldRenderMarkdownAsPlain(src) {
    if (!src) return false;
    if (src.length > 90000) return true;

    let lines = 1;
    let currentLine = 0;
    for (let i = 0; i < src.length; i += 1) {
      if (src.charCodeAt(i) === 10) {
        lines += 1;
        currentLine = 0;
        if (lines > 1200) return true;
      } else {
        currentLine += 1;
        if (currentLine > 2600) return true;
      }
    }

    let markers = 0;
    for (let i = 0; i < src.length; i += 1) {
      const c = src.charCodeAt(i);
      if (c === 42 || c === 95 || c === 126 || c === 96 || c === 91 || c === 93 || c === 35 || c === 62 || c === 124 || c === 10) {
        markers += 1;
        if (markers > 3500) return true;
      }
    }

    return false;
  }

  function renderMarkdownText(text, opts = {}) {
    const srcOriginal = (text ?? '').toString().replace(/\r\n?/g, '\n');
    if (!srcOriginal.trim()) return '';

    if (shouldRenderMarkdownAsPlain(srcOriginal)) {
      return `<div class="msg-md msg-md-safe-plain"><pre class="msg-md-plain">${escapeHtml(srcOriginal)}</pre></div>`;
    }

    try {
      const full = !!opts.full;
      const maxChars = full ? 90000 : 16000;
      const maxLines = full ? 1200 : 420;
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
      const maxBlocks = full ? 600 : 420;
      const pushBlock = (html) => {
        if (blocks.length < maxBlocks) blocks.push(html);
      };
      const skipBlank = () => {
        while (i < lines.length && !lines[i].trim()) i += 1;
      };

      while (i < lines.length && blocks.length < maxBlocks) {
        skipBlank();
        if (i >= lines.length) break;

        const line = lines[i];

        const fence = line.match(/^\s*```+\s*([^`\s]{0,32})?.*$/i);
        if (fence) {
          const lang = (fence[1] || '').trim();
          i += 1;
          const code = [];
          let codeChars = 0;
          const maxCodeChars = full ? 40000 : 12000;

          while (i < lines.length && !/^\s*```+\s*$/.test(lines[i])) {
            const ln = lines[i];
            codeChars += ln.length + 1;
            if (codeChars <= maxCodeChars) code.push(ln);
            i += 1;
          }

          if (i < lines.length && /^\s*```+\s*$/.test(lines[i])) i += 1;
          if (codeChars > maxCodeChars) code.push('…код обрезан для безопасности интерфейса');

          pushBlock(`<pre class="msg-md-pre"${lang ? ` data-lang="${escapeHtml(lang)}"` : ''}><code>${escapeHtml(code.join('\n'))}</code></pre>`);
          continue;
        }

        const h = line.match(/^\s{0,3}(#{1,6})\s+(.+?)\s*#*\s*$/);
        if (h) {
          const level = Math.min(3, h[1].length);
          pushBlock(`<h${level} class="msg-md-h msg-md-h${level}">${renderInlineMarkdownSafe(h[2])}</h${level}>`);
          i += 1;
          continue;
        }

        if (/^\s{0,3}([-*_])\s*\1\s*\1[\s\1]*$/.test(line)) {
          pushBlock('<hr class="msg-md-hr">');
          i += 1;
          continue;
        }

        if (isMarkdownTableStart(lines, i)) {
          const aligns = parseMarkdownTableSeparator(lines[i + 1]) || [];
          const tableRows = [splitMarkdownTableRow(lines[i])];
          i += 2;
          const maxTableRows = full ? 160 : 70;

          while (i < lines.length && lines[i].trim() && lines[i].includes('|') && tableRows.length < maxTableRows) {
            const cells = splitMarkdownTableRow(lines[i]);
            if (cells.length < 2) break;
            tableRows.push(cells);
            i += 1;
          }

          while (i < lines.length && lines[i].trim() && lines[i].includes('|')) i += 1;
          pushBlock(renderMarkdownTableBlock(tableRows, aligns));
          continue;
        }

        if (/^\s{0,3}>\s?/.test(line)) {
          const q = [];
          while (i < lines.length && q.length < 80 && /^\s{0,3}>\s?/.test(lines[i])) {
            q.push(lines[i].replace(/^\s{0,3}>\s?/, ''));
            i += 1;
          }
          while (i < lines.length && /^\s{0,3}>\s?/.test(lines[i])) i += 1;
          pushBlock(`<blockquote class="msg-md-quote">${q.map(renderInlineMarkdownSafe).join('<br>')}</blockquote>`);
          continue;
        }

        if (/^\s{0,3}[-*+]\s+/.test(line)) {
          const items = [];
          while (i < lines.length && items.length < 180) {
            const m = lines[i].match(/^\s{0,3}[-*+]\s+(.+)$/);
            if (!m) break;
            items.push(`<li>${renderInlineMarkdownSafe(m[1])}</li>`);
            i += 1;
          }
          while (i < lines.length && /^\s{0,3}[-*+]\s+/.test(lines[i])) i += 1;
          pushBlock(`<ul class="msg-md-list">${items.join('')}</ul>`);
          continue;
        }

        if (/^\s{0,3}\d{1,4}[.)]\s+/.test(line)) {
          const items = [];
          while (i < lines.length && items.length < 180) {
            const m = lines[i].match(/^\s{0,3}\d{1,4}[.)]\s+(.+)$/);
            if (!m) break;
            items.push(`<li>${renderInlineMarkdownSafe(m[1])}</li>`);
            i += 1;
          }
          while (i < lines.length && /^\s{0,3}\d{1,4}[.)]\s+/.test(lines[i])) i += 1;
          pushBlock(`<ol class="msg-md-list">${items.join('')}</ol>`);
          continue;
        }

        const para = [];
        let paraChars = 0;
        while (i < lines.length && lines[i].trim() && !isMarkdownBlockStart(lines[i])) {
          paraChars += lines[i].length + 1;
          if (para.length < 80 && paraChars <= (full ? 9000 : 4500)) para.push(lines[i]);
          i += 1;
        }

        if (para.length) {
          pushBlock(`<p class="msg-md-p">${para.map(renderInlineMarkdownSafe).join('<br>')}</p>`);
          continue;
        }

        i += 1;
      }

      if (clipped || i < lines.length || blocks.length >= maxBlocks) {
        pushBlock('<div class="msg-md-note">Показан форматированный фрагмент. Ответ слишком длинный для полного inline-рендера.</div>');
      }

      return `<div class="msg-md">${blocks.join('')}</div>`;
    } catch (err) {
      console.warn('[ADMIN HOMIE] markdown fallback', err);
      return `<div class="msg-md msg-md-safe-plain"><pre class="msg-md-plain">${escapeHtml(srcOriginal)}</pre></div>`;
    }
  }

  function injectHomieStyles() {
    if (document.getElementById('admin-homie-chat-style')) return;

    const style = document.createElement('style');
    style.id = 'admin-homie-chat-style';
    style.textContent = `
#homie-center-root.homie-chat-card {
  height: min(72vh, 760px);
  min-height: 540px;
  display: grid;
  grid-template-rows: auto minmax(0, 1fr) auto;
  gap: 0;
  padding: 0 !important;
  overflow: hidden;
  background: #0b1020;
}
.homie-chat-topbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 12px 14px;
  border-bottom: 1px solid var(--border);
  background: #11182d;
  flex-wrap: wrap;
}
.homie-chat-titlebox {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}
.homie-chat-avatar {
  width: 42px;
  height: 42px;
  border-radius: 14px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 900;
  color: #fff;
  background: radial-gradient(circle at 30% 25%, #7c5cff, #1b2850 70%);
  border: 1px solid #4d67b1;
  box-shadow: 0 8px 24px rgba(0,0,0,.22);
  flex: 0 0 auto;
}
.homie-chat-name {
  font-size: 18px;
  font-weight: 900;
  line-height: 1.1;
}
.homie-chat-sub {
  color: var(--muted);
  font-size: 12px;
  margin-top: 3px;
}
.homie-chat-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
#homie-center-feed.homie-chat-feed {
  min-height: 0 !important;
  scrollbar-width: thin;
  scrollbar-color: #5268a8 #0b1020;
  max-height: none !important;
  height: auto !important;
  overflow-y: auto !important;
  display: flex !important;
  flex-direction: column !important;
  gap: 0 !important;
  justify-content: flex-start !important;
  padding: 14px 16px 18px !important;
  border: 0 !important;
  border-radius: 0 !important;
  background: linear-gradient(180deg, #0b1020 0%, #090d18 100%) !important;
  scroll-behavior: smooth;
}
.homie-message {
  display: grid;
  grid-template-columns: 42px minmax(0, 1fr);
  gap: 12px;
  padding: 10px 4px;
  border-radius: 14px;
}
.homie-message:hover {
  background: rgba(255,255,255,.025);
}
.homie-message-avatar {
  width: 42px;
  height: 42px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 900;
  color: #fff;
  background: #172241;
  border: 1px solid #314068;
  flex: 0 0 auto;
}
.homie-message.user .homie-message-avatar {
  background: linear-gradient(180deg, #2a3b70, #172241);
}
.homie-message.assistant .homie-message-avatar {
  background: radial-gradient(circle at 35% 30%, #7c5cff, #16234a 72%);
}
.homie-message.system .homie-message-avatar {
  color: #aecaff;
  background: #121933;
}
.homie-message.error .homie-message-avatar {
  color: #ffb4bf;
  background: #2d1420;
  border-color: #5a2730;
}
.homie-message-main {
  min-width: 0;
  max-width: 100%;
}
.homie-message-head {
  display: flex;
  align-items: baseline;
  gap: 8px;
  min-height: 22px;
}
.homie-message-author {
  font-weight: 900;
  color: #f2f5ff;
}
.homie-message-time {
  color: var(--muted);
  font-size: 12px;
}
.homie-message-body {
  min-width: 0;
  max-width: 100%;
  color: #eef2ff;
  line-height: 1.52;
  overflow-wrap: break-word;
  word-break: break-word;
}
.homie-message.pending .homie-message-body {
  color: var(--muted);
}
.homie-typing {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}
.homie-typing i {
  width: 6px;
  height: 6px;
  border-radius: 999px;
  background: currentColor;
  opacity: .5;
  animation: homieTyping 1s infinite ease-in-out;
}
.homie-typing i:nth-child(2) { animation-delay: .14s; }
.homie-typing i:nth-child(3) { animation-delay: .28s; }
@keyframes homieTyping {
  0%, 80%, 100% { transform: translateY(0); opacity: .35; }
  40% { transform: translateY(-4px); opacity: 1; }
}
.homie-composer {
  border-top: 1px solid var(--border);
  background: #0d1324;
  padding: 12px 14px 14px;
}
#homie-center-input.homie-input {
  display: block;
  width: 100%;
  max-width: none !important;
  min-height: 54px;
  max-height: 180px;
  resize: none;
  border-radius: 16px;
  padding: 13px 14px;
  background: #090d18;
  border: 1px solid #25345a;
  color: var(--text);
  line-height: 1.45;
}
.homie-composer-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  margin-top: 10px;
  flex-wrap: wrap;
}
.homie-composer-hint {
  color: var(--muted);
  font-size: 12px;
}
.msg-md {
  display: block;
  min-width: 0;
  max-width: 100%;
}
.msg-md-p { margin: 0 0 8px 0; }
.msg-md-p:last-child { margin-bottom: 0; }
.msg-md-h {
  margin: 10px 0 8px;
  line-height: 1.2;
  letter-spacing: -.02em;
}
.msg-md-h1 { font-size: 22px; }
.msg-md-h2 { font-size: 19px; }
.msg-md-h3 { font-size: 17px; }
.msg-md-pre {
  position: relative;
  margin: 9px 0;
  padding: 34px 12px 12px;
  border-radius: 14px;
  border: 1px solid #2f3d68;
  background: #060913;
  color: #e8eeff;
  overflow: auto;
  max-width: 100%;
  font: 13px/1.45 ui-monospace, SFMono-Regular, Consolas, 'Liberation Mono', monospace;
  white-space: pre;
}
.msg-md-pre::before {
  content: attr(data-lang);
  position: absolute;
  top: 8px;
  left: 10px;
  max-width: 180px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: #9eb7ff;
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: .06em;
}
.msg-md-pre:not([data-lang])::before {
  content: 'CODE';
}
.msg-md-code {
  padding: 2px 5px;
  border-radius: 6px;
  background: #171f38;
  border: 1px solid #2b3a66;
  color: #e9d5ff;
  font: .92em ui-monospace, SFMono-Regular, Consolas, monospace;
}
.msg-md-list {
  margin: 8px 0;
  padding-left: 24px;
}
.msg-md-list li { margin: 3px 0; }
.msg-md-quote {
  margin: 9px 0;
  padding: 8px 12px;
  border-left: 3px solid #7c5cff;
  background: rgba(124, 92, 255, .08);
  border-radius: 0 10px 10px 0;
  color: #dbe6ff;
}
.msg-md-hr {
  border: 0;
  border-top: 1px solid #2f3d68;
  margin: 12px 0;
}
.msg-md-table-wrap {
  max-width: 100%;
  overflow: auto;
  margin: 10px 0;
  border: 1px solid #2f3d68;
  border-radius: 12px;
}
.msg-md-table {
  width: 100%;
  border-collapse: collapse;
  min-width: 420px;
  background: #0b1020;
}
.msg-md-table th,
.msg-md-table td {
  padding: 8px 10px;
  border-bottom: 1px solid #26365f;
  text-align: left;
  vertical-align: top;
}
.msg-md-table th {
  background: #11182d;
  color: #fff;
  font-weight: 900;
}
.msg-md-note {
  margin-top: 8px;
  color: var(--muted);
  font-size: 12px;
}
.msg-md-plain {
  margin: 0;
  white-space: pre-wrap;
  font: 13px/1.45 ui-monospace, SFMono-Regular, Consolas, monospace;
}
.msg-link {
  color: #9eb7ff;
  text-decoration: none;
}
.msg-link:hover { text-decoration: underline; }
.homie-message-actions {
  display: inline-flex;
  gap: 6px;
  margin-left: auto;
  opacity: 0;
  transition: opacity .14s ease;
}
.homie-message:hover .homie-message-actions {
  opacity: 1;
}
.homie-copy-btn {
  border: 1px solid #2f3d68;
  background: #121933;
  color: #dbe6ff;
  border-radius: 9px;
  padding: 3px 7px;
  font-size: 12px;
  cursor: pointer;
}
.homie-copy-btn:hover { background: #182243; }
@media (max-width: 760px) {
  #homie-center-root.homie-chat-card { min-height: 560px; height: 72vh; }
  .homie-chat-topbar { align-items: flex-start; }
  .homie-message { grid-template-columns: 34px minmax(0, 1fr); gap: 9px; }
  .homie-message-avatar { width: 34px; height: 34px; font-size: 13px; }
  #homie-center-feed.homie-chat-feed { padding: 12px !important; }
}
`;

    document.head.appendChild(style);
  }

  function formatLocalTime() {
    try {
      return new Date().toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit' });
    } catch (_) {
      return '';
    }
  }

  function roleMeta(role) {
    const raw = (role || '').toString().trim();
    if (raw === 'Рома') return { kind: 'user', title: 'Рома', avatar: 'Р' };
    if (raw === 'Homie') return { kind: 'assistant', title: 'Homie', avatar: 'H' };
    if (raw === 'Ошибка') return { kind: 'error', title: 'Ошибка', avatar: '!' };
    return { kind: 'system', title: raw || 'system', avatar: 'i' };
  }

  function makeTypingHtml() {
    return '<span class="homie-typing"><span>думаю</span><i></i><i></i><i></i></span>';
  }

  function addHomieMessage(feed, role, text, opts = {}) {
    const meta = roleMeta(role);
    const box = document.createElement('div');
    box.className = `homie-message ${meta.kind}${opts.pending ? ' pending' : ''}`;

    if (opts.pending) {
      box.innerHTML = `
        <div class="homie-message-avatar">${escapeHtml(meta.avatar)}</div>
        <div class="homie-message-main">
          <div class="homie-message-head">
            <span class="homie-message-author">${escapeHtml(meta.title)}</span>
            <span class="homie-message-time">${escapeHtml(formatLocalTime())}</span>
          </div>
          <div class="homie-message-body">${makeTypingHtml()}</div>
        </div>
      `;
    } else {
      const raw = (text ?? '').toString();
      const rendered = meta.kind === 'error' || meta.kind === 'system'
        ? renderMarkdownText(raw || '')
        : renderMarkdownText(raw || '[пустой ответ]');

      box.innerHTML = `
        <div class="homie-message-avatar">${escapeHtml(meta.avatar)}</div>
        <div class="homie-message-main">
          <div class="homie-message-head">
            <span class="homie-message-author">${escapeHtml(meta.title)}</span>
            <span class="homie-message-time">${escapeHtml(formatLocalTime())}</span>
            <span class="homie-message-actions"><button type="button" class="homie-copy-btn" title="Копировать">⧉</button></span>
          </div>
          <div class="homie-message-body">${rendered}</div>
        </div>
      `;

      const copyBtn = qs('.homie-copy-btn', box);
      if (copyBtn) {
        copyBtn.addEventListener('click', async (e) => {
          e.preventDefault();
          e.stopPropagation();
          try {
            await navigator.clipboard.writeText(raw);
            copyBtn.textContent = '✓';
            setTimeout(() => { copyBtn.textContent = '⧉'; }, 700);
          } catch (_) {}
        });
      }
    }

    feed.appendChild(box);
    feed.scrollTop = feed.scrollHeight;
    return box;
  }

  function prepareHomieLayout(root, feed, input, sendBtn, resetBtn, checkBtn, toolsBtn, statusEl) {
    injectHomieStyles();

    const csrfEl = qs('#homie-center-csrf', root) || qs('#homie-center-csrf');
    const sessionEl = qs('#homie-center-session', root) || qs('#homie-center-session');

    root.classList.add('homie-chat-card');
    root.innerHTML = '';

    if (csrfEl) root.appendChild(csrfEl);
    if (sessionEl) root.appendChild(sessionEl);

    if (feed) {
      feed.removeAttribute('style');
      feed.className = 'homie-chat-feed';
      feed.innerHTML = '';
    }

    if (input) {
      input.removeAttribute('style');
      input.className = 'homie-input';
      input.setAttribute('rows', '1');
      input.setAttribute('placeholder', 'Напиши задачу для Homie...');
      input.setAttribute('autocomplete', 'off');
      input.setAttribute('spellcheck', 'true');
    }

    if (sendBtn) sendBtn.className = 'btn-soft homie-send-btn';
    if (resetBtn) resetBtn.className = 'btn-soft homie-reset-btn';
    if (checkBtn) checkBtn.className = 'btn-soft homie-top-btn';
    if (toolsBtn) toolsBtn.className = 'btn-soft homie-top-btn';
    if (statusEl) statusEl.className = 'pill homie-status-pill';

    const topbar = document.createElement('div');
    topbar.className = 'homie-chat-topbar';
    topbar.innerHTML = `
      <div class="homie-chat-titlebox">
        <div class="homie-chat-avatar">H</div>
        <div class="homie-chat-title-main">
          <div class="homie-chat-name">Homie AI</div>
          <div class="homie-chat-sub">Локальный агент админ-панели</div>
        </div>
      </div>
      <div class="homie-chat-actions" data-homie-top-actions></div>
    `;
    root.appendChild(topbar);

    const topActions = qs('[data-homie-top-actions]', topbar);
    if (topActions) {
      if (statusEl) topActions.appendChild(statusEl);
      if (checkBtn) topActions.appendChild(checkBtn);
      if (toolsBtn) topActions.appendChild(toolsBtn);
    }

    if (feed) root.appendChild(feed);

    const composer = document.createElement('div');
    composer.className = 'homie-composer';
    if (input) composer.appendChild(input);

    const row = document.createElement('div');
    row.className = 'homie-composer-row';
    row.innerHTML = `
      <span class="homie-composer-hint">Enter — отправить · Shift/Ctrl + Enter — новая строка</span>
      <span class="homie-chat-actions" data-homie-composer-actions></span>
    `;
    composer.appendChild(row);

    const composerActions = qs('[data-homie-composer-actions]', row);
    if (composerActions) {
      if (resetBtn) composerActions.appendChild(resetBtn);
      if (sendBtn) composerActions.appendChild(sendBtn);
    }

    root.appendChild(composer);
  }

  function autoresizeTextarea(input) {
    if (!input) return;
    input.style.height = 'auto';
    input.style.height = `${Math.min(Math.max(input.scrollHeight, 54), 180)}px`;
  }

  function initHomieCenter() {
    const root = qs('#homie-center-root');
    if (!root || root.dataset.ready === '1') return;
    root.dataset.ready = '1';

    const csrfEl = qs('#homie-center-csrf');
    const sessionEl = qs('#homie-center-session');
    const feed = qs('#homie-center-feed');
    const input = qs('#homie-center-input');
    const sendBtn = qs('#homie-center-send');
    const resetBtn = qs('#homie-center-reset');
    const checkBtn = qs('#homie-center-check');
    const toolsBtn = qs('#homie-center-tools');
    const statusEl = qs('#homie-center-status');

    if (!csrfEl || !sessionEl || !feed || !input || !sendBtn || !resetBtn || !checkBtn || !toolsBtn) return;

    prepareHomieLayout(root, feed, input, sendBtn, resetBtn, checkBtn, toolsBtn, statusEl);

    const csrf = csrfEl.value;
    const sessionId = sessionEl.value || 'admin-center';

    function setStatus(text, ok) {
      if (!statusEl) return;
      statusEl.textContent = text;
      statusEl.style.borderColor = ok ? '#28533a' : '#5a2730';
      statusEl.style.color = ok ? '#98e2b8' : '#ffb4bf';
      statusEl.style.background = ok ? '#10261b' : '#251316';
    }

    function add(role, text, opts) {
      return addHomieMessage(feed, role, text, opts || {});
    }

    function insertNewline() {
      const start = input.selectionStart || 0;
      const end = input.selectionEnd || 0;
      const before = input.value.slice(0, start);
      const after = input.value.slice(end);
      input.value = before + '\n' + after;
      input.selectionStart = input.selectionEnd = start + 1;
      autoresizeTextarea(input);
    }

    async function readJson(url, options) {
      const res = await fetch(url, Object.assign({
        credentials: 'same-origin',
        cache: 'no-store',
      }, options || {}));

      const text = await res.text();
      let data;
      try {
        data = JSON.parse(text);
      } catch (_) {
        data = { ok: false, error: text || 'bad json' };
      }

      if (!res.ok && data.ok !== true) {
        data.ok = false;
        data.error = data.error || ('HTTP ' + res.status);
      }

      return data;
    }

    function looksLikeJsonText(text) {
      const s = (text || '').trim();
      if (s.length < 2) return false;
      return (s.startsWith('{') && s.endsWith('}'))
        || (s.startsWith('[') && s.endsWith(']'))
        || (s.startsWith('\"') && s.endsWith('\"'));
    }

    function normalizeEscapedPlainText(text) {
      const raw = (text ?? '').toString();
      if (!raw || raw.includes('\n')) return raw;

      const escapedLineBreaks = (raw.match(/\\n|\\r\\n/g) || []).length;
      if (!escapedLineBreaks) return raw;

      return raw
        .replace(/\\r\\n/g, '\n')
        .replace(/\\n/g, '\n')
        .replace(/\\t/g, '  ');
    }

    function homieTextFromValue(value, depth = 0) {
      if (value === null || value === undefined || depth > 6) return '';

      if (typeof value === 'string') {
        const raw = value.trim();
        if (!raw) return '';

        if (looksLikeJsonText(raw)) {
          try {
            const parsed = JSON.parse(raw);
            const nested = homieTextFromValue(parsed, depth + 1);
            if (nested.trim()) return nested;
          } catch (_) {}
        }

        return normalizeEscapedPlainText(value);
      }

      if (Array.isArray(value)) {
        for (const item of value) {
          const text = homieTextFromValue(item, depth + 1);
          if (text.trim()) return text;
        }
        return '';
      }

      if (typeof value === 'object') {
        const keys = ['answer', 'final', 'message', 'content', 'text', 'output', 'response', 'result'];
        for (const key of keys) {
          if (Object.prototype.hasOwnProperty.call(value, key)) {
            const text = homieTextFromValue(value[key], depth + 1);
            if (text.trim()) return text;
          }
        }

        if (Array.isArray(value.choices)) {
          for (const choice of value.choices) {
            const text = homieTextFromValue(choice, depth + 1);
            if (text.trim()) return text;
          }
        }

        return '';
      }

      return String(value);
    }

    function normalizeHomieAnswer(data) {
      return homieTextFromValue(data, 0).trim();
    }

    async function checkHealth(silent) {
      checkBtn.disabled = true;
      try {
        const data = await readJson('/admin/homie/health');
        if (data.ok) {
          const up = data.upstream || {};
          setStatus('Online', true);
          if (!silent) {
            add('system', `Homie online.\nModel: ${up.model || 'unknown'}\nWorkspace: ${up.workspace || 'unknown'}`);
          }
        } else {
          setStatus('Offline', false);
          if (!silent) add('Ошибка', data.error || 'Homie offline');
        }
      } catch (e) {
        setStatus('Offline', false);
        if (!silent) add('Ошибка', String(e));
      } finally {
        checkBtn.disabled = false;
      }
    }

    async function showTools() {
      toolsBtn.disabled = true;
      const pending = add('system', '', { pending: true });
      try {
        const data = await readJson('/admin/homie/tools');
        pending.remove();
        if (data.ok) {
          const tools = (data.upstream && data.upstream.tools) || [];
          add('system', tools.length ? (`## Инструменты Homie\n\n${tools.map((x) => `- ${x}`).join('\n')}`) : 'Homie ответил, но список инструментов пуст.');
        } else {
          add('Ошибка', data.error || 'tools error');
        }
      } catch (e) {
        pending.remove();
        add('Ошибка', String(e));
      } finally {
        toolsBtn.disabled = false;
      }
    }

    async function send() {
      const message = input.value.trim();
      if (!message || sendBtn.disabled) return;

      input.value = '';
      autoresizeTextarea(input);
      add('Рома', message);
      const pending = add('Homie', '', { pending: true });
      sendBtn.disabled = true;

      try {
        const data = await readJson('/admin/homie/chat', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ csrf, session_id: sessionId, message }),
        });

        pending.remove();
        if (data.ok) {
          const answer = normalizeHomieAnswer(data);
          add('Homie', answer || '[пустой ответ от Homie]');
        } else {
          add('Ошибка', data.error || 'unknown error');
        }
      } catch (e) {
        pending.remove();
        add('Ошибка', String(e));
      } finally {
        sendBtn.disabled = false;
        input.focus();
      }
    }

    async function reset() {
      resetBtn.disabled = true;
      try {
        const data = await readJson('/admin/homie/reset', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ csrf, session_id: sessionId, message: '' }),
        });
        feed.innerHTML = '';
        add(data.ok ? 'system' : 'Ошибка', data.ok ? 'Контекст Homie сброшен.' : (data.error || 'reset error'));
      } catch (e) {
        add('Ошибка', String(e));
      } finally {
        resetBtn.disabled = false;
        input.focus();
      }
    }

    sendBtn.addEventListener('click', send);
    resetBtn.addEventListener('click', reset);
    checkBtn.addEventListener('click', () => checkHealth(false));
    toolsBtn.addEventListener('click', showTools);

    input.addEventListener('input', () => autoresizeTextarea(input));
    input.addEventListener('keydown', (e) => {
      if (e.key !== 'Enter') return;

      if (e.ctrlKey || e.shiftKey) {
        e.preventDefault();
        insertNewline();
        return;
      }

      e.preventDefault();
      send();
    });

    autoresizeTextarea(input);
    add('system', 'Homie готов к запросу из админ-панели. Markdown и блоки кода теперь отображаются форматированно.');
    checkHealth(true);
  }

  document.addEventListener('DOMContentLoaded', () => {
    setupPanelButtons();
    setupPersistedInputs();
    setupClearButtons();
    setupMessenger();
    initHomieCenter();
  });
})();
