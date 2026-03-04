let _overlay = null;
let _state = null;

function clamp(v, min, max) {
  return Math.min(max, Math.max(min, v));
}

function ensureOverlay() {
  if (_overlay) return _overlay;

  const overlay = document.createElement('div');
  overlay.id = 'avatarCropOverlay';
  overlay.className = 'modal-overlay hidden';
  overlay.innerHTML = `
    <div class="avatar-crop-modal" role="dialog" aria-modal="true">
      <div class="avatar-crop-header">
        <div class="avatar-crop-title" id="avatarCropTitle">Настройка аватара</div>
        <button class="avatar-crop-close" id="avatarCropClose" type="button" aria-label="Закрыть">✕</button>
      </div>

      <div class="avatar-crop-body">
        <div class="avatar-crop-preview-wrap">
          <div class="avatar-crop-preview" id="avatarCropPreview">
            <img class="avatar-crop-img" id="avatarCropImg" alt="avatar" draggable="false" />
          </div>
          <div class="avatar-crop-hint">Перетащи изображение и выбери масштаб</div>
        </div>

        <div class="avatar-crop-controls">
          <div class="avatar-crop-row">
            <div class="avatar-crop-label">Масштаб</div>
            <input class="avatar-crop-zoom" id="avatarCropZoom" type="range" min="1" max="3" step="0.01" value="1" />
          </div>
        </div>
      </div>

      <div class="avatar-crop-actions">
        <button class="btn btn-secondary" id="avatarCropCancel" type="button">Отмена</button>
        <button class="btn" id="avatarCropSave" type="button">Сохранить</button>
      </div>
    </div>
  `;

  document.body.appendChild(overlay);
  _overlay = overlay;
  overlay.addEventListener('mousedown', (e) => {
    if (e.target === overlay && _state?.resolve) {
      e.preventDefault();
      _state.resolve(null);
      close();
    }
  });

  const closeBtn = overlay.querySelector('#avatarCropClose');
  closeBtn.addEventListener('click', () => {
    if (_state?.resolve) {
      _state.resolve(null);
    }
    close();
  });

  const cancelBtn = overlay.querySelector('#avatarCropCancel');
  cancelBtn.addEventListener('click', () => {
    if (_state?.resolve) {
      _state.resolve(null);
    }
    close();
  });

  const saveBtn = overlay.querySelector('#avatarCropSave');
  saveBtn.addEventListener('click', async () => {
    if (!_state) return;
    try {
      const blob = await renderToBlob();
      _state.resolve(blob);
    } catch (err) {
      console.error('[AVATAR_CROP] render error', err);
      _state.resolve(null);
    }
    close();
  });

  const zoomEl = overlay.querySelector('#avatarCropZoom');
  zoomEl.addEventListener('input', () => {
    if (!_state) return;
    const z = parseFloat(zoomEl.value || '1');
    setZoom(z);
  });

  const preview = overlay.querySelector('#avatarCropPreview');
  preview.addEventListener('pointerdown', (e) => {
    if (!_state) return;
    if (!_state.ready) return;
    e.preventDefault();

    preview.setPointerCapture(e.pointerId);
    _state.dragging = true;
    _state.lastX = e.clientX;
    _state.lastY = e.clientY;
  });

  preview.addEventListener('pointermove', (e) => {
    if (!_state?.dragging) return;
    e.preventDefault();

    const dx = e.clientX - _state.lastX;
    const dy = e.clientY - _state.lastY;
    _state.lastX = e.clientX;
    _state.lastY = e.clientY;

    _state.offsetX += dx;
    _state.offsetY += dy;
    clampOffsets();
    applyTransform();
  });

  const stopDrag = (e) => {
    if (!_state) return;
    _state.dragging = false;
    try {
      preview.releasePointerCapture(e.pointerId);
    } catch (_) {}
  };

  preview.addEventListener('pointerup', stopDrag);
  preview.addEventListener('pointercancel', stopDrag);

  document.addEventListener('keydown', (e) => {
    if (!_state) return;
    if (_overlay.classList.contains('hidden')) return;
    if (e.key === 'Escape') {
      if (_state.resolve) _state.resolve(null);
      close();
    }
  });

  return overlay;
}

function close() {
  if (!_overlay) return;
  _overlay.classList.add('hidden');
  document.body.classList.remove('no-scroll');
  if (_state?.objectUrl) {
    try { URL.revokeObjectURL(_state.objectUrl); } catch (_) {}
  }

  _state = null;
}

function clampOffsets() {
  const C = _state.container;
  const w = _state.displayW;
  const h = _state.displayH;

  if (w <= C) {
    _state.offsetX = (C - w) / 2;
  } else {
    _state.offsetX = clamp(_state.offsetX, C - w, 0);
  }

  if (h <= C) {
    _state.offsetY = (C - h) / 2;
  } else {
    _state.offsetY = clamp(_state.offsetY, C - h, 0);
  }
}

function applyTransform() {
  const img = _overlay.querySelector('#avatarCropImg');
  img.style.width = `${_state.displayW}px`;
  img.style.height = `${_state.displayH}px`;
  img.style.transform = `translate(${_state.offsetX}px, ${_state.offsetY}px)`;
}

function setZoom(zoom) {
  const C = _state.container;
  const oldW = _state.displayW;
  const oldH = _state.displayH;

  const relX = (C / 2 - _state.offsetX) / oldW;
  const relY = (C / 2 - _state.offsetY) / oldH;

  _state.zoom = zoom;

  const base = _state.baseScale;
  const scale = base * zoom;

  _state.displayW = Math.round(_state.imgW * scale);
  _state.displayH = Math.round(_state.imgH * scale);

  _state.offsetX = C / 2 - relX * _state.displayW;
  _state.offsetY = C / 2 - relY * _state.displayH;

  clampOffsets();
  applyTransform();
}

async function renderToBlob() {
  const outSize = 256;
  const C = _state.container;
  const scaleOut = outSize / C;

  const canvas = document.createElement('canvas');
  canvas.width = outSize;
  canvas.height = outSize;

  const ctx = canvas.getContext('2d');
  if (!ctx) throw new Error('Canvas 2D context missing');

  const img = _overlay.querySelector('#avatarCropImg');
  const dx = _state.offsetX * scaleOut;
  const dy = _state.offsetY * scaleOut;
  const dw = _state.displayW * scaleOut;
  const dh = _state.displayH * scaleOut;

  ctx.clearRect(0, 0, outSize, outSize);
  ctx.drawImage(img, dx, dy, dw, dh);

  return await new Promise((resolve) => {
    canvas.toBlob(
      (b) => resolve(b),
      'image/png',
      0.92,
    );
  });
}

export async function openAvatarCropper(file, options = {}) {
  ensureOverlay();

  const title = options.title || 'Настройка аватара';
  const titleEl = _overlay.querySelector('#avatarCropTitle');
  titleEl.textContent = title;

  const imgEl = _overlay.querySelector('#avatarCropImg');
  const zoomEl = _overlay.querySelector('#avatarCropZoom');

  zoomEl.value = '1';
  imgEl.removeAttribute('src');

  document.body.classList.add('no-scroll');
  _overlay.classList.remove('hidden');

  const containerSize = 240;

  return await new Promise((resolve) => {
    _state = {
      resolve,
      ready: false,
      objectUrl: null,
      container: containerSize,
      imgW: 0,
      imgH: 0,
      baseScale: 1,
      zoom: 1,
      displayW: 0,
      displayH: 0,
      offsetX: 0,
      offsetY: 0,
      dragging: false,
      lastX: 0,
      lastY: 0,
    };

    const objectUrl = URL.createObjectURL(file);
    _state.objectUrl = objectUrl;
    imgEl.src = objectUrl;

    const onLoad = async () => {
      imgEl.removeEventListener('load', onLoad);

      const w = imgEl.naturalWidth || 1;
      const h = imgEl.naturalHeight || 1;
      _state.imgW = w;
      _state.imgH = h;

      const base = Math.max(containerSize / w, containerSize / h);
      _state.baseScale = base;
      _state.zoom = 1;

      _state.displayW = Math.round(w * base);
      _state.displayH = Math.round(h * base);

      _state.offsetX = (containerSize - _state.displayW) / 2;
      _state.offsetY = (containerSize - _state.displayH) / 2;

      clampOffsets();
      applyTransform();
      _state.ready = true;
    };

    imgEl.addEventListener('load', onLoad);
  });
}
