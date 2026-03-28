// /static/js/voice.js
// Minimal WebRTC voice UI for LaBerry (website).
// Uses WS signaling: voice_join/voice_leave + rtc_offer/rtc_answer/rtc_candidate.

export function initVoice({ wsManager, api, getMe }) {
  if (!wsManager || typeof wsManager.send !== 'function') {
    console.warn('[VOICE] wsManager is missing');
    return;
  }
  if (typeof api !== 'function') {
    console.warn('[VOICE] api() is missing');
    return;
  }

  // --- UI elements ---
  const elVoiceBar = document.getElementById('voiceBar');
  const elChanName = document.getElementById('voiceChannelName');
  const elStatus = document.getElementById('voiceStatusText');
  const elPeers = document.getElementById('voicePeersList');
  const btnMute = document.getElementById('voiceMuteBtn');
  const btnDeafen = document.getElementById('voiceDeafenBtn');
  const btnShare = document.getElementById('voiceShareBtn');
  const btnLeave = document.getElementById('voiceLeaveBtn');
  const elAudioSink = document.getElementById('voiceAudioSink');
  const elVideoSink = document.getElementById('voiceVideoSink');

  // Voice bar stream notice
  const elStreamNotice = document.getElementById('voiceStreamNotice');
  const elStreamText = document.getElementById('voiceStreamText');
  const btnStreamWatch = document.getElementById('voiceStreamWatchBtn');

  // Voice view (Discord-like stage)
  const voiceView = document.getElementById('voiceView');
  const voiceViewChanName = document.getElementById('voiceViewChannelName');
  const voiceViewState = document.getElementById('voiceViewState');
  const voiceStage = document.getElementById('voiceStage');
  const voiceStageVideoWrap = document.getElementById('voiceStageVideoWrap');
  const voiceStageVideo = document.getElementById('voiceStageVideo');
  const voicePeerTile = document.getElementById('voicePeerTile');
  const voicePeerAva = document.getElementById('voicePeerAva');
  const voicePeerNameEl = document.getElementById('voicePeerName');
  const voiceStageTop = document.getElementById('voiceStageTop');
  const voiceLiveBadge = document.getElementById('voiceLiveBadge');
  const voiceStageName = document.getElementById('voiceStageName');
  const voiceStageEmpty = document.getElementById('voiceStageEmpty');
  const voiceStageWatchBtn = document.getElementById('voiceStageWatchBtn');
  const voiceStageStrip = document.getElementById('voiceStageStrip');
  const voiceSelfPreview = document.getElementById('voiceSelfPreview');
  const voiceSelfVideo = document.getElementById('voiceSelfVideo');
  const vcMuteBtn = document.getElementById('vcMuteBtn');
  const vcDeafenBtn = document.getElementById('vcDeafenBtn');
  const vcShareBtn = document.getElementById('vcShareBtn');
  const vcLeaveBtn = document.getElementById('vcLeaveBtn');
  const voicePipBtn = document.getElementById('voicePipBtn');
  const voiceFullscreenBtn = document.getElementById('voiceFullscreenBtn');

  // Screen share modal
  const ssOverlay = document.getElementById('screenShareOverlay');
  const ssCloseBtn = document.getElementById('screenShareCloseBtn');
  const ssCancelBtn = document.getElementById('screenShareCancelBtn');
  const ssStartBtn = document.getElementById('screenShareStartBtn');
  const ssPreviewVideo = document.getElementById('ssPreviewVideo');
  const ssPreviewHint = document.getElementById('ssPreviewHint');
  const ssAudioChk = document.getElementById('ssAudioChk');

  // Remote viewer panel
  const ssPanel = document.getElementById('ssPanel');
  const ssPanelTitle = document.getElementById('ssPanelTitle');
  const ssRemoteVideo = document.getElementById('ssRemoteVideo');
  const ssPanelHideBtn = document.getElementById('ssPanelHideBtn');
  const ssPanelFullscreenBtn = document.getElementById('ssPanelFullscreenBtn');

// Voice members dock is inside the voice stage (right panel is used for text chat in voice mode)
const voiceMembersDock = document.getElementById('voiceMembersDock');
let voiceMembersGrid = null;
let voiceMembersCount = null;

function ensureVoiceMembersSection() {
  if (!voiceMembersDock) return;
  if (voiceMembersGrid && voiceMembersCount) return;
  voiceMembersGrid = document.getElementById('voiceMembersGrid');
  voiceMembersCount = document.getElementById('voiceMembersCount');
}


  // --- state ---
  const pcs = new Map();          // peerId -> RTCPeerConnection
  const remoteStreams = new Map(); // peerId -> MediaStream
  const audioEls = new Map();     // peerId -> HTMLAudioElement
  const remoteVideoStreams = new Map(); // peerId -> MediaStream
  const videoEls = new Map();     // peerId -> HTMLVideoElement (optional sink)
  const nameCache = new Map();    // userId -> username

  let meId = null;
  let meName = null;

  let iceConfig = null;
  let localStream = null;
  let localStreamError = null;

  // screenshare state
  let screenStream = null;
  let ssWatchers = new Set(); // userIds who are watching OUR share
  let liveSharers = new Set(); // userIds who currently broadcast screen

  let screenVideoTrack = null;
  let screenAudioTrack = null;
  let screenSenders = new Map(); // peerId -> { videoSender, audioSender }
  let isSharingScreen = false;
  let ssSelectedSurface = 'monitor';
  let ssSelectedRes = 720;
  let ssSelectedFps = 30;
  let ssIncludeAudio = false;
  
  let audioCtx = null;
  let analyser = null;
  
  let lastJoinAttemptChannelId = null;

  let inChannelId = null;
  let inChannelName = null;

  let muted = false;
  let deafened = false;

  let joining = false;

  // screen share viewing state (what is shown in the big stage)
  let remoteShareUserId = null;
  let remoteShareStream = null;
  let watchingUserId = null;
  let watchingStream = null;
  let focusedUserId = null; // local UI focus in members tiles
  let stagePriorityMode = 'stream'; // stream | user

  function applyStagePriorityMode(mode) {
    stagePriorityMode = (mode === 'user') ? 'user' : 'stream';
    if (!voiceStage) return;
    voiceStage.classList.remove('mode-stream', 'mode-user');
    voiceStage.classList.add(stagePriorityMode === 'user' ? 'mode-user' : 'mode-stream');
  }


  function setBarVisible(v) {
    if (!elVoiceBar) return;
    elVoiceBar.hidden = !v;
    document.body.classList.toggle('voice-muted', !!muted && v);
    document.body.classList.toggle('voice-deafened', !!deafened && v);
  }

  function isVoiceViewOpen() {
    return !!voiceView && !voiceView.hidden;
  }

  function updateVoiceViewHeader() {
    if (voiceViewChanName) voiceViewChanName.textContent = (inChannelName || 'Voice').toString();
    if (!voiceViewState) return;
    if (!inChannelId) {
      voiceViewState.textContent = 'Не подключено';
      return;
    }
    const s = (elStatus?.textContent || '').toLowerCase();
    if (joining || s.includes('подключ')) {
      voiceViewState.textContent = 'Подключение...';
    } else {
      voiceViewState.textContent = 'Подключено';
    }
  }

  function setStatus(text) {
    if (elStatus) elStatus.textContent = text || '';
    updateVoiceViewHeader();
  }

  function setChannelName(name) {
    if (elChanName) elChanName.textContent = name || 'Voice';
    updateVoiceViewHeader();
  }

  function setPeersText(text) {
    if (!elPeers) return;
    elPeers.textContent = text || '';
  }

  function markVoiceChannelInList(channelId) {
    const list = document.getElementById('channels-list');
    if (!list) return;
    const items = list.querySelectorAll('.item.channel.voice');
    items.forEach((it) => {
      const id = Number(it.dataset.channelId);
      it.classList.toggle('voice-joined', Number.isFinite(id) && id === channelId);
    });
  }


  function renderUsersUnderVoiceChannel(channelId, ids) {
    const list = document.getElementById('channels-list');
    if (!list) return;

    // Clear all containers first
    try {
      list.querySelectorAll('.item.channel.voice .voice-users').forEach((el) => {
        el.innerHTML = '';
        el.hidden = true;
      });
    } catch (_) {}

    const cid = Number(channelId);
    if (!Number.isFinite(cid) || cid <= 0) return;

    const item = list.querySelector(`.item.channel.voice[data-channel-id="${cid}"]`);
    const box = item?.querySelector?.('.voice-users');
    if (!box) return;

    const arr = Array.isArray(ids) ? ids : [];
    if (!arr.length) {
      box.innerHTML = '';
      box.hidden = true;
      return;
    }

    const frag = document.createDocumentFragment();
    for (const uid of arr) {
      const name = (nameCache.get(uid) || (uid === meId ? (meName || 'Вы') : `User#${uid}`)).toString();
      const letter = (name.charAt(0) || 'U').toUpperCase();

      const row = document.createElement('div');
      row.className = 'voice-user' + (uid === meId ? ' me' : '');
      row.innerHTML = `<div class="vua">${escapeHtml(letter)}</div><div class="vun" title="${escapeHtml(name)}">${escapeHtml(uid === meId ? 'Вы' : name)}</div>`;
      frag.appendChild(row);
    }

    box.innerHTML = '';
    box.appendChild(frag);
    box.hidden = false;
  }

  async function ensureMe() {
    if (meId && meName) return { id: meId, name: meName };

    try {
      const m = (typeof getMe === 'function') ? getMe() : null;
      if (m && typeof m === 'object') {
        const id = Number(m.id);
        if (Number.isFinite(id) && id > 0) meId = id;
        meName = (m.nickname || m.username || '').toString() || null;
      }
    } catch (_) {}

    if (!meId) {
      try {
        const raw = localStorage.getItem('user_id');
        const id = raw ? Number(raw) : null;
        if (Number.isFinite(id) && id > 0) meId = id;
      } catch (_) {}
    }

    if (!meId || !meName) {
      try {
        const me = await api('/api/users/me');
        const id = Number(me?.id);
        if (Number.isFinite(id) && id > 0) meId = id;
        meName = (me?.nickname || me?.username || '').toString() || meName;
      } catch (_) {}
    }

    if (meId && meName) nameCache.set(meId, meName);
    return { id: meId, name: meName };
  }

  async function resolveName(userId) {
    const id = Number(userId);
    if (!Number.isFinite(id) || id <= 0) return 'Unknown';

    if (nameCache.has(id)) return nameCache.get(id);

    // try members list DOM cache
    try {
      const el = document.querySelector(`.member[data-user-id="${id}"]`);
      const fromData = el?.dataset?.username;
      if (fromData) {
        nameCache.set(id, fromData);
        return fromData;
      }
      const nameEl = el?.querySelector?.('.name');
      const t = nameEl?.textContent?.trim();
      if (t) {
        nameCache.set(id, t);
        return t;
      }
    } catch (_) {}

    // fallback: API
    try {
      const u = await api(`/api/users/${id}`);
      const name = (u?.username || '').toString() || `User#${id}`;
      nameCache.set(id, name);
      return name;
    } catch (_) {
      return `User#${id}`;
    }
  }

  async function ensureIceConfig() {
    if (iceConfig) return iceConfig;
    try {
      const r = await api('/api/rtc/ice');
      const servers = r?.iceServers || r?.ice_servers || [];
      if (Array.isArray(servers) && servers.length) {
        iceConfig = { iceServers: servers };
      } else {
        iceConfig = { iceServers: [{ urls: ['stun:stun.l.google.com:19302'] }] };
      }
    } catch (_) {
      iceConfig = { iceServers: [{ urls: ['stun:stun.l.google.com:19302'] }] };
    }
    return iceConfig;
  }

  async function getLocalStreamOptional() {
  if (localStream) return localStream;
  if (localStreamError) return null;

  // Microphone access requires a secure context (HTTPS or localhost)
  if (!window.isSecureContext) {
    localStreamError = new Error('secure_context_required');
    return null;
  }
  if (!navigator.mediaDevices?.getUserMedia) {
    localStreamError = new Error('getUserMedia_not_supported');
    return null;
  }

  const constraints = {
    audio: {
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true
    }
  };

  try {
    localStream = await navigator.mediaDevices.getUserMedia(constraints);

    // create audio context & analyzer
    audioCtx = new (window.AudioContext || window.webkitAudioContext)();
    const source = audioCtx.createMediaStreamSource(localStream);
    analyser = audioCtx.createAnalyser();
    analyser.fftSize = 1024;
    source.connect(analyser);

    localStreamError = null;
    return localStream;
  } catch (err) {
    localStreamError = err;
    return null;
  }
}

  function stopLocalStream() {
    try {
      if (localStream) {
        localStream.getTracks().forEach((t) => t.stop());
      }
    } catch (_) {}

    try {
      if (audioCtx) {
        audioCtx.close();
      }
    } catch (_) {}
    audioCtx = null;
    analyser = null;

    localStream = null;
    localStreamError = null;
  }

  function closePeer(peerId) {
    const id = Number(peerId);
    const pc = pcs.get(id);
    if (pc) {
      try { pc.onicecandidate = null; } catch (_) {}
      try { pc.ontrack = null; } catch (_) {}
      try { pc.onconnectionstatechange = null; } catch (_) {}
      try { pc.close(); } catch (_) {}
    }
    pcs.delete(id);

    const s = remoteStreams.get(id);
    if (s) {
      try { s.getTracks().forEach((t) => t.stop()); } catch (_) {}
    }
    remoteStreams.delete(id);

    const a = audioEls.get(id);
    if (a) {
      try { a.srcObject = null; } catch (_) {}
      try { a.remove(); } catch (_) {}
    }
    audioEls.delete(id);

    const vs = remoteVideoStreams.get(id);
    if (vs) {
      try { vs.getTracks().forEach((t) => t.stop()); } catch (_) {}
    }
    remoteVideoStreams.delete(id);

    const ve = videoEls.get(id);
    if (ve) {
      try { ve.srcObject = null; } catch (_) {}
      try { ve.remove(); } catch (_) {}
    }
    videoEls.delete(id);

    // if this peer was sharing — clear
    try {
      if (remoteShareUserId === id) {
        hideRemoteViewerPanel();
      }
      if (watchingUserId === id) {
        if (isSharingScreen && screenStream) stageShowSelfShare();
        else stageShowEmpty();
      }
    } catch (_) {}
  }

  function cleanupAllPeers() {
    [...pcs.keys()].forEach((id) => closePeer(id));
  }

function getVoiceDisplayName(uid) {
  const id = Number(uid);
  if (!Number.isFinite(id) || id <= 0) return 'Unknown';
  if (id === meId) return (meName || 'Вы').toString();
  return (nameCache.get(id) || `User#${id}`).toString();
}

function getVoiceAvatarInnerHtml(uid, name) {
  const id = Number(uid);
  // Try to reuse server members avatar HTML if present
  try {
    const el = document.querySelector(`.member[data-user-id="${id}"] .avatar`);
    const html = el?.innerHTML;
    if (html) return html;
  } catch (_) {}

  const n = (name || getVoiceDisplayName(id)).toString();
  const letter = (n.charAt(0) || 'U').toUpperCase();
  return escapeHtml(letter);
}

function renderVoiceMembersTiles(ids) {
  ensureVoiceMembersSection();
  if (!voiceMembersDock || !voiceMembersGrid || !voiceMembersCount) return;

  const arr = Array.isArray(ids) ? ids : [];
  if (!inChannelId || !arr.length) {
    voiceMembersGrid.innerHTML = '';
    voiceMembersDock.hidden = true;
    if (voiceMembersCount) voiceMembersCount.textContent = '(0)';
    return;
  }

  // Hide the dock when you are alone in the channel (keeps the stage clean)
  if (arr.length <= 1) {
    voiceMembersGrid.innerHTML = '';
    voiceMembersDock.hidden = true;
    if (voiceMembersCount) voiceMembersCount.textContent = `(${arr.length})`;
    return;
  }

  voiceMembersDock.hidden = false;
  voiceMembersCount.textContent = `(${arr.length})`;

  const frag = document.createDocumentFragment();

  for (const uid of arr) {
    const id = Number(uid);
    if (!Number.isFinite(id) || id <= 0) continue;

    const name = getVoiceDisplayName(id);
    const tile = document.createElement('div');
    tile.className = 'voice-member-tile' + (id === meId ? ' me' : '') + (focusedUserId === id ? ' focused' : '');
    tile.dataset.userId = String(id);

    const avaHtml = getVoiceAvatarInnerHtml(id, name);
    const isLive = (id === meId ? (isSharingScreen && !!screenStream) : liveSharers.has(id));

    tile.innerHTML = `
      <div class="voice-member-ava">${avaHtml}</div>
      <div class="voice-member-meta">
        <div class="voice-member-name" title="${escapeHtml(name)}">${escapeHtml(id === meId ? 'Вы' : name)}</div>
        <div class="voice-member-sub">${id === meId ? 'Вы' : (isLive ? 'Демонстрация' : 'Участник')}</div>
      </div>
      ${isLive ? '<div class="voice-member-live">LIVE</div>' : ''}
    `;

    // Right click toggles local focus
    tile.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      e.stopPropagation();

      if (focusedUserId === id) focusedUserId = null;
      else focusedUserId = id;

      // Re-render to update highlight
      renderVoiceMembersTiles(arr);

      // Apply focus to stage if possible
      applyVoiceFocusToStage();
    });

    // Left click: if LIVE — toggle watch; else open user menu if available
    tile.addEventListener('click', (e) => {
      try {
        const isLive = (id === meId ? (isSharingScreen && !!screenStream) : liveSharers.has(id));
        if (isLive && id !== meId) {
          e.preventDefault();
          e.stopPropagation();
          toggleWatchShare(id).catch(() => {});
          return;
        }
      } catch (_) {}

      const fn = window.lbShowUserMenu;
      if (typeof fn !== 'function') return;
      if (id === meId) return;
      const anchor = tile.querySelector('.voice-member-ava') || tile;
      try {
        fn({
          userId: id,
          username: name,
          anchorEl: anchor,
          allowDm: true,
          allowAddFriend: true,
          allowRemoveFriend: false,
        });
      } catch (_) {}
    });

    frag.appendChild(tile);

    // Resolve name async if unknown
    if (!nameCache.has(id) && id !== meId) {
      resolveName(id).then(() => {
        // Re-render if still in same channel and the tile exists
        try {
          if (!inChannelId) return;
          renderVoiceMembersTiles(arr);
        } catch (_) {}
      }).catch(() => {});
    }
  }

  voiceMembersGrid.innerHTML = '';
  voiceMembersGrid.appendChild(frag);
}



  function currentVoiceMemberIds() {
    const ids = [meId, ...pcs.keys()].filter((n) => Number.isFinite(n) && n > 0);
    ids.sort((a, b) => a - b);
    return ids;
  }

  function ssSend(type, toUserId) {
    const uid = Number(toUserId);
    if (!inChannelId) return;
    if (!Number.isFinite(uid) || uid <= 0) return;
    try {
      wsManager.send({
        type,
        data: {
          channel_id: inChannelId,
          to_user_id: uid,
        }
      });
    } catch (_) {}
  }

  async function watchShare(userId) {
    const uid = Number(userId);
    if (!inChannelId) return;
    if (!Number.isFinite(uid) || uid <= 0) return;
    if (uid === meId) return;

    // stop previous watch
    if (watchingUserId && watchingUserId !== uid) {
      ssSend('voice_ss_unwatch', watchingUserId);
      try {
        if (remoteShareUserId === watchingUserId) hideRemoteScreenShare();
      } catch (_) {}
    }

    watchingUserId = uid;
    focusedUserId = uid;

    // request stream from the sharer
    ssSend('voice_ss_watch', uid);

    // UI update
    try { renderVoiceMembersTiles(currentVoiceMemberIds()); } catch (_) {}
    try {
      if (isVoiceViewOpen() && !(isSharingScreen && screenStream)) {
        stageShowEmpty();
        if (voiceStageName) voiceStageName.textContent = 'Подключение...';
      }
    } catch (_) {}
  }

  async function unwatchShare(userId) {
    const uid = Number(userId);
    if (!Number.isFinite(uid) || uid <= 0) return;

    ssSend('voice_ss_unwatch', uid);

    if (watchingUserId === uid) watchingUserId = null;
    if (focusedUserId === uid) focusedUserId = null;

    try {
      if (remoteShareUserId === uid) hideRemoteScreenShare();
    } catch (_) {}

    try { renderVoiceMembersTiles(currentVoiceMemberIds()); } catch (_) {}

    if (isVoiceViewOpen()) {
      if (isSharingScreen && screenStream) stageShowSelfShare();
      else stageShowEmpty();
    }
  }

  async function toggleWatchShare(userId) {
    const uid = Number(userId);
    if (!Number.isFinite(uid) || uid <= 0) return;
    if (watchingUserId === uid) return unwatchShare(uid);
    return watchShare(uid);
  }

function applyVoiceFocusToStage() {
  // If no voice view open — focus is only visual in the sidebar
  if (!isVoiceViewOpen()) return;

  const fid = Number(focusedUserId || 0);
  if (!Number.isFinite(fid) || fid <= 0) {
    // restore default stage
    if (isSharingScreen && screenStream) {
      stageShowSelfShare();
    } else if (remoteShareStream && remoteShareUserId) {
      stageShowRemoteShare().catch(() => stageShowEmpty());
    } else {
      stageShowEmpty();
    }
    return;
  }

  if (fid === meId && isSharingScreen && screenStream) {
    stageShowSelfShare();
    return;
  }

  const s = remoteVideoStreams.get(fid);
  if (s) {
    stageSetStream(s, fid);
    resolveName(fid)
      .then((nm) => { if (voiceStageName) voiceStageName.textContent = nm; })
      .catch(() => { if (voiceStageName) voiceStageName.textContent = `User#${fid}`; });
    if (voiceStageTop) voiceStageTop.hidden = false;
    return;
  }

  // No video stream — keep default stage, but keep the focus highlight.
  if (isSharingScreen && screenStream) {
    stageShowSelfShare();
  } else if (remoteShareStream && remoteShareUserId) {
    stageShowRemoteShare().catch(() => stageShowEmpty());
  } else {
    stageShowEmpty();
  }
}


  function updateVoiceUiPeers() {
    // Hide peer chips in the left voicebar (users expect the list under the voice channel).
    try {
      if (elPeers) {
        elPeers.innerHTML = '';
        elPeers.style.display = 'none';
      }
    } catch (_) {}

    // Hide stage strip (no users list inside the stage).
    try {
      if (voiceStageStrip) {
        voiceStageStrip.innerHTML = '';
        voiceStageStrip.style.display = 'none';
      }
    } catch (_) {}

    if (!inChannelId) {
      focusedUserId = null;
      renderUsersUnderVoiceChannel(null, []);
      renderVoiceMembersTiles([]);
      updateVoiceStageLetter();
      updateStageWatchButtons();
      applyVoiceFocusToStage();
      return;
    }

    const peerIds = [...pcs.keys()].sort((a, b) => a - b);
    const count = peerIds.length + 1;

    setStatus(`В голосе: ${count}`);

    const ids = [meId, ...peerIds].filter((n) => Number.isFinite(n) && n > 0);
    renderUsersUnderVoiceChannel(inChannelId, ids);
    renderVoiceMembersTiles(ids);

    // If focused user left the call — drop focus
    try {
      if (focusedUserId && !ids.includes(focusedUserId)) focusedUserId = null;
    } catch (_) {}

    updateVoiceStageLetter();
    updateStageWatchButtons();
    applyVoiceFocusToStage();
  }

  function escapeHtml(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/\"/g, "&quot;")
      .replace(/'/g, "&#039;");
  }

  function updateVoiceStageLetter() {
    if (!voiceStageEmpty) return;
    const n = (meName || inChannelName || 'U').toString().trim();
    const letter = (n.charAt(0) || 'U').toUpperCase();
    voiceStageEmpty.dataset.letter = letter;
  }

  function stageSetStream(stream, ownerUserId) {
    const hasVideo = !!stream;

    if (hasVideo && stagePriorityMode !== 'user') {
      applyStagePriorityMode('stream');
    }

    // Set/clear the stage video
    try {
      if (voiceStageVideo) {
        if (hasVideo) {
          voiceStageVideo.hidden = false;
          voiceStageVideo.srcObject = stream;
          const p = voiceStageVideo.play?.();
          if (p && typeof p.catch === 'function') p.catch(() => {});
        } else {
          try { voiceStageVideo.pause?.(); } catch (_) {}
          voiceStageVideo.srcObject = null;
          voiceStageVideo.hidden = true;
        }
      }
    } catch (_) {}

    // Speaker tile next to the stream (keep it even without stream to avoid an "empty" left column)
    try {
      if (voicePeerTile) {
        const uid = hasVideo ? Number(ownerUserId || 0) : Number(meId || 0);
        const show = uid > 0;
        voicePeerTile.hidden = !show;
        if (show) {
          const cached = nameCache.get(uid);
          const nm = (uid === meId ? (meName || 'Вы') : (cached || `User#${uid}`)).toString();
          if (voicePeerNameEl) voicePeerNameEl.textContent = nm;
          const letter = (nm.trim().charAt(0) || 'U').toUpperCase();
          if (voicePeerAva) voicePeerAva.textContent = letter;
        }
      }
    } catch (_) {}

    // Stage state classes for CSS layout
    try {
      if (voiceStage) {
        voiceStage.classList.toggle('has-stream', hasVideo);
        voiceStage.classList.toggle('no-stream', !hasVideo);
      }
    } catch (_) {}

    // Show placeholder whenever there is no stream (keeps UI clean)
    try {
      if (voiceStageEmpty) {
        const showPlaceholder = !hasVideo;
        voiceStageEmpty.hidden = !showPlaceholder;
      }
    } catch (_) {}

    if (voiceLiveBadge) voiceLiveBadge.hidden = !hasVideo;
  }

  async function stageShowRemoteShare() {
    if (!remoteShareStream || !remoteShareUserId) {
      stageSetStream(null, null);
      updateStageWatchButtons();
      return;
    }

    stageSetStream(remoteShareStream, remoteShareUserId);

    if (voiceStageName) {
      const nm = await resolveName(remoteShareUserId).catch(() => `User#${remoteShareUserId}`);
      voiceStageName.textContent = nm;
      try {
        if (voicePeerTile && !voicePeerTile.hidden && voicePeerNameEl) voicePeerNameEl.textContent = nm;
        const letter = (nm.trim().charAt(0) || 'U').toUpperCase();
        if (voicePeerAva) voicePeerAva.textContent = letter;
      } catch (_) {}
    }
    if (voiceStageTop) voiceStageTop.hidden = false;
    updateStageWatchButtons();
  }

  function stageShowSelfShare() {
    if (!screenStream) return;
    stageSetStream(screenStream, meId);
    if (voiceStageName) voiceStageName.textContent = 'Вы транслируете';
    if (voiceStageTop) voiceStageTop.hidden = false;
    updateStageWatchButtons();
  }

  function stageShowEmpty() {
    stageSetStream(null, null);
    if (voiceStageName) voiceStageName.textContent = '';
    if (voiceStageTop) voiceStageTop.hidden = false;
    updateStageWatchButtons();
  }

  function updateStageWatchButtons() {
    const hasRemote = !!remoteShareStream && !!remoteShareUserId;
    if (elStreamNotice) elStreamNotice.hidden = !hasRemote;
    // "Watch" button is kept only for backward compatibility; UI uses the stage window.
    if (voiceStageWatchBtn) voiceStageWatchBtn.hidden = true;
    if (btnStreamWatch) btnStreamWatch.hidden = true;
  }

  function updateVoiceStageStrip(ids) {
    if (!voiceStageStrip) return;
    const arr = Array.isArray(ids) ? ids : [];
    if (!arr.length) {
      voiceStageStrip.innerHTML = '';
      return;
    }

    const frag = document.createDocumentFragment();
    for (const uid of arr) {
      const name = (nameCache.get(uid) || (uid === meId ? (meName || 'Вы') : `User#${uid}`)).toString();
      const letter = (name.charAt(0) || 'U').toUpperCase();

      const it = document.createElement('div');
      it.className = 'voice-strip-item' + (uid === meId ? ' me' : '');
      it.innerHTML = `<div class="voice-strip-ava">${escapeHtml(letter)}</div><div class="voice-strip-name" title="${escapeHtml(name)}">${escapeHtml(name)}</div>`;
      frag.appendChild(it);
    }

    voiceStageStrip.innerHTML = '';
    voiceStageStrip.appendChild(frag);
  }

  // ==============================
  // SCREEN SHARE UI + HELPERS
  // ==============================

  function setSharingUi(v) {
    document.body.classList.toggle('voice-sharing', !!v && !!inChannelId);
  }
  function showRemoteViewerPanel() {
    if (!ssPanel || !ssRemoteVideo) return;
    if (!remoteShareStream || !remoteShareUserId) return;
    try { ssRemoteVideo.pause?.(); } catch (_) {}
    try { ssRemoteVideo.srcObject = remoteShareStream; } catch (_) {}
    try { ssRemoteVideo.play?.().catch(() => {}); } catch (_) {}
    try {
      const title = (elStreamText?.textContent || '').toString().trim();
      if (ssPanelTitle) ssPanelTitle.textContent = title || 'Демонстрация';
    } catch (_) {}
    try { ssPanel.hidden = false; } catch (_) {}
  }

  function hideRemoteViewerPanel() {
    try { if (ssPanel) ssPanel.hidden = true; } catch (_) {}
    try { if (ssRemoteVideo) ssRemoteVideo.srcObject = null; } catch (_) {}
  }


  function hideRemoteScreenShare() {
    remoteShareUserId = null;
    remoteShareStream = null;

    if (elStreamText) elStreamText.textContent = 'Демонстрация';
    if (elStreamNotice) elStreamNotice.hidden = true;
    if (voiceStageWatchBtn) voiceStageWatchBtn.hidden = true;

    // hide viewer panel
    hideRemoteViewerPanel();

    if (isSharingScreen && screenStream) stageShowSelfShare();
    else stageShowEmpty();
  }

  async function showRemoteScreenShare(fromUserId, stream) {
    const uid = Number(fromUserId);
    if (!Number.isFinite(uid) || uid <= 0) return;

    remoteShareUserId = uid;
    remoteShareStream = stream;

    const name = await resolveName(uid).catch(() => `User#${uid}`);
    if (elStreamText) elStreamText.textContent = `Демонстрация: ${name}`;
    if (elStreamNotice) elStreamNotice.hidden = false;

    // preload viewer panel (but keep hidden until user clicks 'Смотреть')
    try { if (ssRemoteVideo) ssRemoteVideo.srcObject = stream; } catch (_) {}

    // if the voice view is open and you are not actively streaming yourself — show immediately
    if (isVoiceViewOpen() && !(isSharingScreen && screenStream)) {
      await stageShowRemoteShare();
    } else {
      updateStageWatchButtons();
    }
  }

  function ssOverlayVisible(v) {
    if (!ssOverlay) return;
    ssOverlay.hidden = !v;
    document.body.classList.toggle('modal-open', !!v);
  }

  function ssReadUiSelections() {
    try {
      const surfaceBtn = ssOverlay?.querySelector?.('.ss-tab.active');
      const surface = (surfaceBtn?.dataset?.surface || 'monitor').toString();
      ssSelectedSurface = surface;
    } catch (_) {}

    try {
      const res = Number(ssOverlay?.querySelector?.('input[name="ssRes"]:checked')?.value || 720);
      if (Number.isFinite(res)) ssSelectedRes = res;
    } catch (_) {}

    try {
      const fps = Number(ssOverlay?.querySelector?.('input[name="ssFps"]:checked')?.value || 30);
      if (Number.isFinite(fps)) ssSelectedFps = fps;
    } catch (_) {}

    try {
      ssIncludeAudio = !!ssAudioChk?.checked;
    } catch (_) {}
  }

  function ssResToDims(res) {
    const r = Number(res);
    if (r === 480) return { width: 854, height: 480 };
    if (r === 1080) return { width: 1920, height: 1080 };
    return { width: 1280, height: 720 };
  }


function ssMaxBitrateKbps(res, fps) {
  const r = Number(res) || 720;
  const f = Math.max(1, Math.min(60, Number(fps) || 30));
  // User requirement: cap 1080p at 6000 kbps.
  if (r >= 1080) return 6000;
  if (r >= 720) return (f >= 60 ? 4500 : 3000);
  return (f >= 60 ? 2500 : 1500);
}

async function ssApplySenderBitrate(sender, res, fps) {
  if (!sender || typeof sender.getParameters !== 'function' || typeof sender.setParameters !== 'function') return;
  const maxKbps = ssMaxBitrateKbps(res, fps);
  const maxBps = Math.max(1, Math.floor(maxKbps * 1000));

  try {
    const p = sender.getParameters() || {};
    if (!p.encodings) p.encodings = [{}];
    if (!Array.isArray(p.encodings) || p.encodings.length === 0) p.encodings = [{}];

    // Per spec/browser behavior: encodings[0].maxBitrate is bps.
    p.encodings[0].maxBitrate = maxBps;
    p.encodings[0].maxFramerate = Math.max(1, Math.min(60, Number(fps) || 30));

    // Prefer keeping resolution for screen share.
    if (!p.degradationPreference) p.degradationPreference = 'maintain-resolution';

    await sender.setParameters(p);
  } catch (e) {
    // Some browsers/implementations may reject parameters; ignore silently.
    // console.warn('[SS] setParameters failed', e);
  }
}

async function ssApplyBitrateToAllPeers() {
  if (!isSharingScreen) return;
  ssReadUiSelections();
  const res = ssSelectedRes;
  const fps = ssSelectedFps;

  for (const entry of screenSenders.values()) {
    if (entry?.videoSender) {
      await ssApplySenderBitrate(entry.videoSender, res, fps);
    }
  }
}

  function ssBuildDisplayConstraints() {
    ssReadUiSelections();
    const dims = ssResToDims(ssSelectedRes);
    const fps = Math.max(1, Math.min(60, Number(ssSelectedFps) || 30));
    const surface = (ssSelectedSurface || 'monitor').toString();
    const includeAudio = !!ssIncludeAudio;

    // Chrome supports privacy controls (ignored by other browsers).
    // https://developer.chrome.com/docs/web-platform/screen-sharing-controls
    const constraints = {
      video: {
        width: { ideal: dims.width },
        height: { ideal: dims.height },
        frameRate: { ideal: fps, max: fps },
        displaySurface: surface, // "monitor" | "window" | "browser" (Chrome)
      },
      audio: includeAudio ? true : false,
      surfaceSwitching: 'include',
      monitorTypeSurfaces: 'include',
      systemAudio: includeAudio ? 'include' : 'exclude',
      selfBrowserSurface: surface === 'browser' ? 'include' : 'exclude',
    };

    // preferCurrentTab is mutually exclusive with selfBrowserSurface: "exclude"
    if (surface === 'browser') {
      constraints.preferCurrentTab = true;
    }

    return constraints;
  }

  function ssSetPreview(stream) {
    if (!ssPreviewVideo) return;
    try {
      ssPreviewVideo.srcObject = stream;
      ssPreviewVideo.autoplay = true;
      ssPreviewVideo.playsInline = true;
      ssPreviewVideo.muted = true;
      ssPreviewVideo.play?.().catch(() => {});
    } catch (_) {}

    if (ssPreviewHint) ssPreviewHint.style.display = stream ? 'none' : '';
  }

  function ssClearPreview() {
    if (!ssPreviewVideo) return;
    try { ssPreviewVideo.pause?.(); } catch (_) {}
    try { ssPreviewVideo.srcObject = null; } catch (_) {}
    if (ssPreviewHint) ssPreviewHint.style.display = '';
  }

  async function ssRequestRenegotiation(peerId) {
    const pid = Number(peerId);
    if (!Number.isFinite(pid) || pid <= 0) return;
    if (!inChannelId) return;

    if (shouldInitiate(pid)) {
      // we are initiator -> create offer immediately
      setTimeout(() => { sendOffer(pid).catch((e) => console.warn('[SS] sendOffer failed', e)); }, 20);
      return;
    }

    // ask initiator peer to create an offer
    wsManager.send({
      type: 'rtc_negotiate',
      data: {
        channel_id: inChannelId,
        to_user_id: pid,
      }
    });
  }

  async function ssAttachToPeer(peerId) {
    const pid = Number(peerId);
    if (!Number.isFinite(pid) || pid <= 0) return;
    const pc = pcs.get(pid);
    if (!pc) return;

    const prev = screenSenders.get(pid) || { videoSender: null, audioSender: null };

    // Video
    if (screenVideoTrack) {
      const already = pc.getSenders().find((s) => s && s.track && s.track.kind === 'video' && s.track === screenVideoTrack);
      if (!already) {
        try {
          prev.videoSender = pc.addTrack(screenVideoTrack, screenStream);
          try { await ssApplySenderBitrate(prev.videoSender, ssSelectedRes, ssSelectedFps); } catch (_) {}
        } catch (e) {
          console.warn('[SS] addTrack(video) failed', e);
        }
      }
    }

    // Audio (tab/system) optional
    if (screenAudioTrack) {
      const alreadyA = pc.getSenders().find((s) => s && s.track && s.track.kind === 'audio' && s.track === screenAudioTrack);
      if (!alreadyA) {
        try {
          prev.audioSender = pc.addTrack(screenAudioTrack, screenStream);
        } catch (e) {
          console.warn('[SS] addTrack(audio) failed', e);
        }
      }
    }

    screenSenders.set(pid, prev);
    await ssRequestRenegotiation(pid);
  }

  async function ssDetachFromPeer(peerId) {
    const pid = Number(peerId);
    if (!Number.isFinite(pid) || pid <= 0) return;
    const pc = pcs.get(pid);
    if (!pc) return;

    const entry = screenSenders.get(pid);
    if (entry?.videoSender) {
      try { pc.removeTrack(entry.videoSender); } catch (_) {}
      try { entry.videoSender.replaceTrack?.(null); } catch (_) {}
    }
    if (entry?.audioSender) {
      try { pc.removeTrack(entry.audioSender); } catch (_) {}
      try { entry.audioSender.replaceTrack?.(null); } catch (_) {}
    }
    screenSenders.delete(pid);

    await ssRequestRenegotiation(pid);
  }

  async function startScreenShare() {
    if (!inChannelId) {
      setStatus('Нужно зайти в голосовой канал');
      return;
    }
    if (isSharingScreen) return;

    // Screen capture requires HTTPS or localhost
    if (!window.isSecureContext) {
      setStatus('Демонстрация экрана требует HTTPS (secure context).');
      return;
    }
    if (!navigator.mediaDevices?.getDisplayMedia) {
      setStatus('getDisplayMedia не поддерживается браузером');
      return;
    }

    const constraints = ssBuildDisplayConstraints();

    try {
      screenStream = await navigator.mediaDevices.getDisplayMedia(constraints);
    } catch (e) {
      console.warn('[SS] getDisplayMedia failed', e);
      setStatus('Демонстрация не запущена');
      return;
    }

    screenVideoTrack = screenStream.getVideoTracks()[0] || null;
    screenAudioTrack = screenStream.getAudioTracks()[0] || null;

    if (!screenVideoTrack) {
      try { screenStream.getTracks().forEach((t) => t.stop()); } catch (_) {}
      screenStream = null;
      setStatus('Нет видео-трека (отказано?)');
      return;
    }

    // Preview in modal (and keep it for “Stop” menu)
    ssSetPreview(screenStream);

    // Auto-stop when user clicks "Stop sharing" in browser UI
    try {
      screenVideoTrack.onended = () => {
        stopScreenShare().catch(() => {});
      };
    } catch (_) {}

    isSharingScreen = true;
    setSharingUi(true);

    // Ensure voice UI is visible for the streamer.
    openVoiceViewAndShowStage();

    // show local preview inside the stage
    try {
      if (voiceSelfPreview && voiceSelfVideo) {
        voiceSelfVideo.srcObject = screenStream;
        voiceSelfVideo.muted = true;
        voiceSelfVideo.play?.().catch(() => {});
        voiceSelfPreview.hidden = false;
      }
    } catch (_) {}

    // if voice view is open — show your stream immediately
    if (isVoiceViewOpen()) {
      stageShowSelfShare();
    } else {
      updateStageWatchButtons();
    }

    // Notify others that we started sharing (stream will be sent only to watchers)
    try { wsManager.send({ type: 'voice_ss_start', data: { channel_id: inChannelId } }); } catch (_) {}
    try { if (meId) liveSharers.add(meId); } catch (_) {}
    updateVoiceUiPeers();

    // Close modal
    ssOverlayVisible(false);

  }

  async function stopScreenShare() {
    if (!isSharingScreen) return;

    // Detach tracks first (renegotiate)
    const peerIds = [...screenSenders.keys()];
    for (const pid of peerIds) {
      await ssDetachFromPeer(pid);
    }
    ssWatchers = new Set();

    try { screenVideoTrack && (screenVideoTrack.onended = null); } catch (_) {}

    try {
      if (screenStream) {
        screenStream.getTracks().forEach((t) => t.stop());
      }
    } catch (_) {}

    screenStream = null;
    screenVideoTrack = null;
    screenAudioTrack = null;
    screenSenders = new Map();
    isSharingScreen = false;
    setSharingUi(false);

    // Notify others that we stopped sharing
    try { wsManager.send({ type: 'voice_ss_stop', data: { channel_id: inChannelId } }); } catch (_) {}
    try { if (meId) liveSharers.delete(meId); } catch (_) {}
    updateVoiceUiPeers();

    try {
      if (voiceSelfPreview) voiceSelfPreview.hidden = true;
      if (voiceSelfVideo) voiceSelfVideo.srcObject = null;
    } catch (_) {}

    if (isVoiceViewOpen()) {
      if (remoteShareStream && remoteShareUserId) {
        stageShowRemoteShare().catch(() => stageShowEmpty());
      } else {
        stageShowEmpty();
      }
    } else {
      updateStageWatchButtons();
    }

    // keep preview clean
    ssClearPreview();
  }

  async function ssApplyQualityIfSharing() {
    if (!isSharingScreen || !screenVideoTrack) return;
    ssReadUiSelections();
    const dims = ssResToDims(ssSelectedRes);
    const fps = Math.max(1, Math.min(60, Number(ssSelectedFps) || 30));

    try {
      await screenVideoTrack.applyConstraints({
        width: { ideal: dims.width },
        height: { ideal: dims.height },
        frameRate: { ideal: fps, max: fps },
      });
    } catch (e) {
      console.warn('[SS] applyConstraints failed', e);
    }

    await ssApplyBitrateToAllPeers();

    // renegotiate (some browsers need it when constraints change)
    const peerIds = [...pcs.keys()];
    for (const pid of peerIds) {
      await ssRequestRenegotiation(pid);
    }
  }

  function setDeafened(v) {
    deafened = !!v;

    // mute all remote audios locally
    for (const a of audioEls.values()) {
      try { a.muted = deafened; } catch (_) {}
    }

    document.body.classList.toggle('voice-deafened', deafened && !!inChannelId);
  }

  function setMuted(v) {
    muted = !!v;
    if (localStream) {
      localStream.getAudioTracks().forEach((t) => (t.enabled = !muted));
    }
    document.body.classList.toggle('voice-muted', muted && !!inChannelId);
  }

  function shouldInitiate(peerId) {
    const p = Number(peerId);
    if (!Number.isFinite(p) || p <= 0) return false;
    if (!meId) return false;
    // deterministic: smaller userId initiates
    return meId < p;
  }

  async function ensurePeerConnection(peerId) {
    const id = Number(peerId);
    if (!Number.isFinite(id) || id <= 0) return null;

    if (pcs.has(id)) return pcs.get(id);

    const config = await ensureIceConfig();
    const pc = new RTCPeerConnection(config);

    pcs.set(id, pc);

    // Always be ready to RECEIVE video (screen share)
    try {
      pc.addTransceiver('video', { direction: 'recvonly' });
    } catch (_) {}

    pc.onicecandidate = (ev) => {
      if (!ev.candidate) return;
      if (!inChannelId) return;

      wsManager.send({
        type: 'rtc_candidate',
        data: {
          channel_id: inChannelId,
          to_user_id: id,
          candidate: ev.candidate
        }
      });
    };

    pc.ontrack = (ev) => {
      const stream = ev.streams && ev.streams[0] ? ev.streams[0] : null;
      if (!stream) return;

      const kind = ev.track && ev.track.kind ? String(ev.track.kind) : '';

      if (kind === 'video') {
        remoteVideoStreams.set(id, stream);
        try {
          ev.track.onended = () => {
            try {
              if (remoteShareUserId === id) hideRemoteScreenShare();
            } catch (_) {}
          };
        } catch (_) {}

        if (watchingUserId === id) {
          showRemoteScreenShare(id, stream);
        }
        return;
      }

      // default: audio
      remoteStreams.set(id, stream);

      let a = audioEls.get(id);
      if (!a) {
        a = document.createElement('audio');
        a.autoplay = true;
        a.playsInline = true;
        a.muted = deafened;
        audioEls.set(id, a);
        if (elAudioSink) elAudioSink.appendChild(a);
        else document.body.appendChild(a);
      }
      a.srcObject = stream;

      updateVoiceUiPeers();
    };

    pc.onconnectionstatechange = () => {
      // keep status readable
      updateVoiceUiPeers();
    };
    // add local tracks (optional)
    const ls = await getLocalStreamOptional();
    if (ls) {
      for (const track of ls.getTracks()) {
        pc.addTrack(track, ls);
      }
    }


    // warm up names async
    resolveName(id).then((n) => {
      nameCache.set(id, n);
      updateVoiceUiPeers();
    }).catch(() => {});

    updateVoiceUiPeers();

    return pc;
  }

  async function sendOffer(peerId) {
    const pc = await ensurePeerConnection(peerId);
    if (!pc || !inChannelId) return;

    // already negotiated
    if (pc.signalingState !== 'stable') return;

    const offer = await pc.createOffer({
      offerToReceiveAudio: true,
      offerToReceiveVideo: true
    });

    await pc.setLocalDescription(offer);

    wsManager.send({
      type: 'rtc_offer',
      data: {
        channel_id: inChannelId,
        to_user_id: Number(peerId),
        sdp: pc.localDescription
      }
    });
  }

  async function handleOffer(fromUserId, sdp) {
    const pc = await ensurePeerConnection(fromUserId);
    if (!pc) return;

    // if we are not stable, rollback is needed for perfect negotiation.
    // keep it simple: accept the remote offer only if stable; otherwise ignore (rare with deterministic initiator).
    if (pc.signalingState !== 'stable') {
      console.warn('[VOICE] glare detected, ignoring offer', { fromUserId, state: pc.signalingState });
      return;
    }

    await pc.setRemoteDescription(new RTCSessionDescription(sdp));

    const answer = await pc.createAnswer();
    await pc.setLocalDescription(answer);

    wsManager.send({
      type: 'rtc_answer',
      data: {
        channel_id: inChannelId,
        to_user_id: Number(fromUserId),
        sdp: pc.localDescription
      }
    });
  }

  async function handleAnswer(fromUserId, sdp) {
    const id = Number(fromUserId);
    const pc = pcs.get(id);
    if (!pc) return;
    await pc.setRemoteDescription(new RTCSessionDescription(sdp));
  }

  async function handleCandidate(fromUserId, candidate) {
    const id = Number(fromUserId);
    const pc = pcs.get(id);
    if (!pc) return;

    try {
      await pc.addIceCandidate(new RTCIceCandidate(candidate));
    } catch (e) {
      console.warn('[VOICE] addIceCandidate failed', e);
    }
  }

  async function tryConnectWs() {
    if (wsManager.isConnected) return true;
    if (typeof wsManager.connect !== 'function') return false;
    const token = localStorage.getItem('auth_token');
    if (!token) return false;
    try {
      await wsManager.connect(token);
      return wsManager.isConnected;
    } catch (e) {
      console.warn('[VOICE] ws connect failed', e);
      return false;
    }
  }

async function join(channelId, channelName) {
  if (!channelId) return;
  if (joining) return;

  // already in this channel: do NOT toggle/leave on repeated click
  if (inChannelId === channelId) {
    // Keep UI visible; just refresh labels/state
    if (channelName) inChannelName = channelName || inChannelName;
    setBarVisible(true);
    setChannelName(inChannelName || channelName || 'Voice');
    updateVoiceViewHeader();
    return;
  }

  joining = true;
  try {
    // if we are in another channel — leave first
    if (inChannelId && inChannelId !== channelId) {
      await leave();
    }

    await ensureMe();

    lastJoinAttemptChannelId = channelId;
    inChannelId = channelId;
    inChannelName = channelName || 'Voice';
    markVoiceChannelInList(channelId);
    setBarVisible(true);
    setStatus('Подключение…');

    updateVoiceStageLetter();
    stageShowEmpty();
    updateVoiceViewHeader();

    // 1) prepare ICE + WS first (so we can show "connecting" state even if mic fails)
    await ensureIceConfig();
    const wsOk = await tryConnectWs();
    if (!wsOk) {
      setStatus('WS не подключен (проверь /ws и токен)');
      return;
    }

    // 2) request mic if possible (optional)
    const ls = await getLocalStreamOptional();
    if (!ls) {
      if (!window.isSecureContext) {
        setStatus('Микрофон требует HTTPS (secure context). Можно только слушать.');
      } else if (!navigator.mediaDevices?.getUserMedia) {
        setStatus('getUserMedia не поддерживается. Можно только слушать.');
      } else {
        setStatus('Нет доступа к микрофону. Можно только слушать.');
      }
    }

    // 3) join voice channel (will be queued until WS auth if needed)
    wsManager.send({ type: 'voice_join', data: { channel_id: inChannelId } });
  } catch (e) {
    console.error('[VOICE] join failed', e);
    setStatus('Ошибка подключения');
    // не прячем панель — пусть видно причину
  } finally {
    joining = false;
  }
}
  async function leave() {
    if (!inChannelId) return;

    // notify server
    try {
      wsManager.send({
        type: 'voice_leave',
        data: { channel_id: inChannelId }
      });
    } catch (_) {}

    // local cleanup (server ack is async)
    localLeaveCleanup();
  }

  function localLeaveCleanup() {
    const prevChannelId = inChannelId;
    cleanupAllPeers();
    stopLocalStream();
    stopScreenShare().catch(() => {});
    hideRemoteScreenShare();

    liveSharers = new Set();
    ssWatchers = new Set();
    watchingUserId = null;


    inChannelId = null;
    inChannelName = null;
    lastJoinAttemptChannelId = null;

    setChannelName('Voice');
    setStatus('');
    setPeersText('');
    setBarVisible(false);
    markVoiceChannelInList(null);
    setSharingUi(false);

    // Clear user lists under the voice channel and tiles
    try { if (prevChannelId) renderUsersUnderVoiceChannel(prevChannelId, []); } catch (_) {}
    try { renderVoiceMembersTiles([]); } catch (_) {}

    // Inform UI that we left voice (so it doesn't look like we are still in voice).
    try {
      if (prevChannelId) {
        document.dispatchEvent(new CustomEvent('lb:voiceLeft', { detail: { channel_id: prevChannelId } }));
      }
    } catch (_) {}

    try {
      if (voiceSelfPreview) voiceSelfPreview.hidden = true;
      if (voiceSelfVideo) voiceSelfVideo.srcObject = null;
    } catch (_) {}

    stageShowEmpty();
    updateVoiceViewHeader();
  }

  async function onVoiceEvent(msg) {
    if (!msg || typeof msg.type !== 'string') return;

    const t = msg.type;

    if (t === 'voice_joined') {
      const ch = Number(msg.channel_id);
      if (!Number.isFinite(ch) || ch <= 0) return;

      // Guard against stale "joined" events after we already left.
      // Accept only if this join was requested/expected by the client.
      if (ch !== Number(inChannelId || 0) && ch !== Number(lastJoinAttemptChannelId || 0)) {
        return;
      }

      // if server joined other (race), accept it
      inChannelId = ch;
      if (!inChannelName) inChannelName = `Voice #${ch}`;

      setBarVisible(true);
      setChannelName(inChannelName);
      markVoiceChannelInList(inChannelId);

      // Inform UI that we joined voice (to unlock voice text chat).
      try {
        document.dispatchEvent(new CustomEvent('lb:voiceJoined', { detail: { channel_id: inChannelId, channel_name: inChannelName || '' } }));
      } catch (_) {}

      const peers = Array.isArray(msg.peers) ? msg.peers : [];
      const shares = Array.isArray(msg.screen_shares) ? msg.screen_shares : [];
      try {
        liveSharers = new Set(shares.map((x) => Number(x)).filter((n) => Number.isFinite(n) && n > 0));
        if (isSharingScreen && meId) liveSharers.add(meId);
      } catch (_) {
        liveSharers = new Set();
      }

      setStatus(`В голосе: ${peers.length + 1}`);

      updateVoiceStageLetter();
      if (!(isSharingScreen && screenStream)) stageShowEmpty();
      updateVoiceViewHeader();

      // pre-cache own name
      if (meId && meName) nameCache.set(meId, meName);

      // ensure PCs for each peer
      for (const pid of peers) {
        const peerId = Number(pid);
        if (!Number.isFinite(peerId) || peerId <= 0) continue;
        if (peerId === meId) continue;

        await ensurePeerConnection(peerId);
      }

      // deterministic offers
      for (const pid of peers) {
        const peerId = Number(pid);
        if (!Number.isFinite(peerId) || peerId <= 0) continue;
        if (peerId === meId) continue;
        if (shouldInitiate(peerId)) {
          // slight delay helps to avoid ICE race in some browsers
          setTimeout(() => { sendOffer(peerId).catch((e) => console.warn('[VOICE] sendOffer failed', e)); }, 40);
        }
      }

      // async names
      const allIds = [meId, ...peers.map((x) => Number(x)).filter((n) => Number.isFinite(n) && n > 0)];
      for (const id of allIds) {
        if (!id) continue;
        resolveName(id).then((n) => {
          nameCache.set(id, n);
          updateVoiceUiPeers();
        }).catch(() => {});
      }

      updateVoiceUiPeers();
      return;
    }

    if (t === 'voice_peer_joined') {
      const ch = Number(msg.channel_id);
      const uid = Number(msg.user_id);
      if (!inChannelId || ch !== inChannelId) return;
      if (!Number.isFinite(uid) || uid <= 0) return;
      if (uid === meId) return;

      await ensurePeerConnection(uid);

      if (shouldInitiate(uid)) {
        setTimeout(() => { sendOffer(uid).catch((e) => console.warn('[VOICE] sendOffer failed', e)); }, 40);
      }

      resolveName(uid).then((n) => {
        nameCache.set(uid, n);
        updateVoiceUiPeers();
      }).catch(() => {});

      updateVoiceUiPeers();
      return;
    }

    if (t === 'voice_peer_left') {
      const ch = Number(msg.channel_id);
      const uid = Number(msg.user_id);
      if (!inChannelId || ch !== inChannelId) return;

      closePeer(uid);
      updateVoiceUiPeers();
      return;
    }

    if (t === 'voice_left') {
      // we left (or switched)
      localLeaveCleanup();
      return;
    }

    if (t === 'rtc_offer') {
      const ch = Number(msg.channel_id);
      if (!inChannelId || ch !== inChannelId) return;
      const from = Number(msg.from_user_id);
      const sdp = msg.sdp;
      if (!from || !sdp) return;

      await handleOffer(from, sdp);
      return;
    }

    if (t === 'rtc_answer') {
      const ch = Number(msg.channel_id);
      if (!inChannelId || ch !== inChannelId) return;
      const from = Number(msg.from_user_id);
      const sdp = msg.sdp;
      if (!from || !sdp) return;

      await handleAnswer(from, sdp);
      return;
    }

    if (t === 'rtc_candidate') {
      const ch = Number(msg.channel_id);
      if (!inChannelId || ch !== inChannelId) return;
      const from = Number(msg.from_user_id);
      const cand = msg.candidate;
      if (!from || !cand) return;

      await handleCandidate(from, cand);
      return;
    }

    if (t === 'rtc_negotiate') {
      const ch = Number(msg.channel_id);
      if (!inChannelId || ch !== inChannelId) return;
      const from = Number(msg.from_user_id);
      if (!from) return;

      // deterministic: only initiator sends offers
      if (shouldInitiate(from)) {
        setTimeout(() => { sendOffer(from).catch((e) => console.warn('[SS] renegotiate sendOffer failed', e)); }, 20);
      }
      return;
    }


    if (t === 'voice_ss_started') {
      const ch = Number(msg.channel_id);
      const uid = Number(msg.user_id);
      if (!inChannelId || ch !== inChannelId) return;
      if (!Number.isFinite(uid) || uid <= 0) return;
      try { liveSharers.add(uid); } catch (_) {}
      updateVoiceUiPeers();
      return;
    }

    if (t === 'voice_ss_stopped') {
      const ch = Number(msg.channel_id);
      const uid = Number(msg.user_id);
      if (!inChannelId || ch !== inChannelId) return;
      if (!Number.isFinite(uid) || uid <= 0) return;
      try { liveSharers.delete(uid); } catch (_) {}

      // If we were watching this user — stop watching and clear stage
      if (watchingUserId === uid) {
        watchingUserId = null;
        focusedUserId = null;
        try { if (remoteShareUserId === uid) hideRemoteScreenShare(); } catch (_) {}
        if (isVoiceViewOpen()) {
          if (isSharingScreen && screenStream) stageShowSelfShare();
          else stageShowEmpty();
        }
      }

      updateVoiceUiPeers();
      return;
    }

    if (t === 'voice_ss_watch') {
      const ch = Number(msg.channel_id);
      const from = Number(msg.from_user_id);
      if (!inChannelId || ch !== inChannelId) return;
      if (!Number.isFinite(from) || from <= 0) return;

      // Someone wants to watch our share
      if (isSharingScreen && screenStream && screenVideoTrack) {
        try { ssWatchers.add(from); } catch (_) {}
        ensurePeerConnection(from).then(() => {
          ssAttachToPeer(from).catch(() => {});
        }).catch(() => {});
      }
      return;
    }

    if (t === 'voice_ss_unwatch') {
      const ch = Number(msg.channel_id);
      const from = Number(msg.from_user_id);
      if (!inChannelId || ch !== inChannelId) return;
      if (!Number.isFinite(from) || from <= 0) return;

      try { ssWatchers.delete(from); } catch (_) {}
      ssDetachFromPeer(from).catch(() => {});
      return;
    }

    // errors from server that affect voice
    if (t === 'error') {
      const code = (msg && msg.code) ? String(msg.code) : '';
      const ch = Number(msg && msg.channel_id);

      // Voice errors always come with channel_id on server side
      if (!Number.isFinite(ch) || ch <= 0) return;

      // Ignore non-related errors
      if (inChannelId && ch !== inChannelId && ch !== lastJoinAttemptChannelId) return;
      if (!inChannelId && lastJoinAttemptChannelId && ch !== lastJoinAttemptChannelId) return;

      console.warn('[VOICE] server error', msg);

      if (code === 'not_voice_channel') setStatus('Это не голосовой канал');
      else if (code === 'not_member') setStatus('Нет доступа к голосовому каналу');
      else if (code === 'not_in_voice') setStatus('Вы не в голосовом канале');
      else setStatus('Ошибка голосового канала');
    }
  }

  // --- UI wiring ---
  if (btnMute) {
    btnMute.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      setMuted(!muted);
    });
  }

  if (btnDeafen) {
    btnDeafen.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      setDeafened(!deafened);
    });
  }

  if (btnLeave) {
    btnLeave.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      leave().catch(() => {});
    });
  }

  function openVoiceViewForChannel() {
    if (!inChannelId) return;
    try {
      document.dispatchEvent(new CustomEvent('lb:openVoiceView', {
        detail: {
          channel_id: inChannelId,
          channel_name: inChannelName || 'Voice'
        }
      }));
    } catch (_) {}
  }

  function openVoiceViewAndShowStage() {
    if (!inChannelId) return;

    openVoiceViewForChannel();

    // wait a tick (voice view becomes visible in app.js)
    setTimeout(() => {
      // Stage is the primary viewer now.
      if (remoteShareStream && remoteShareUserId) {
        stageShowRemoteShare().catch(() => {});
      } else if (isSharingScreen && screenStream) {
        stageShowSelfShare();
      } else {
        stageShowEmpty();
      }
    }, 60);
  }

  if (btnStreamWatch) {
    btnStreamWatch.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      openVoiceViewAndShowStage();
    });
  }

  if (voicePeerTile) {
    voicePeerTile.title = 'Показать участника крупнее';
    voicePeerTile.addEventListener('click', (e) => {
      if (e.target && e.target.closest && e.target.closest('button, a')) return;
      applyStagePriorityMode('user');
    });
  }

  if (voiceStageVideoWrap) {
    voiceStageVideoWrap.title = 'Показать демонстрацию крупнее';
    voiceStageVideoWrap.addEventListener('click', (e) => {
      if (e.target && e.target.closest && e.target.closest('button, a')) return;
      applyStagePriorityMode('stream');
    });
  }

  applyStagePriorityMode('stream');

  if (voiceStageWatchBtn) {
    voiceStageWatchBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      openVoiceViewAndShowStage();
    });
  }

  // Make the voice view accessible for the streamer too: clicking the voice bar opens the voice UI.
  try {
    const main = elVoiceBar?.querySelector?.('.voicebar-main') || elVoiceBar;
    if (main) {
      main.addEventListener('click', (e) => {
        // Do not react to action buttons clicks.
        if (e?.target?.closest?.('.voicebar-actions')) return;
        openVoiceViewAndShowStage();
      });
    }
  } catch (_) {}

  // Voice view controls (bottom bar)
  if (vcMuteBtn) {
    vcMuteBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      setMuted(!muted);
    });
  }
  if (vcDeafenBtn) {
    vcDeafenBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      setDeafened(!deafened);
    });
  }
  if (vcShareBtn) {
    vcShareBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      openScreenShareModal();
    });
  }
  if (vcLeaveBtn) {
    vcLeaveBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      leave().catch(() => {});
    });
  }

  // PIP / fullscreen (stage)
  if (voicePipBtn) {
    voicePipBtn.addEventListener('click', async (e) => {
      e.preventDefault();
      e.stopPropagation();
      const v = voiceStageVideo;
      if (!v || !v.srcObject) return;
      try {
        if (document.pictureInPictureElement) {
          await document.exitPictureInPicture();
        } else if (v.requestPictureInPicture) {
          await v.requestPictureInPicture();
        }
      } catch (_) {}
    });
  }

  if (voiceFullscreenBtn) {
    voiceFullscreenBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      const el = voiceStage || voiceStageVideo;
      if (!el) return;
      try {
        if (document.fullscreenElement) document.exitFullscreen?.();
        else el.requestFullscreen?.();
      } catch (_) {}
    });
  }

  // --- Screen share UI wiring ---
  function openScreenShareModal() {
    if (!ssOverlay) return;

    // reflect current state
    if (isSharingScreen) {
      if (ssStartBtn) ssStartBtn.textContent = 'Остановить';
      if (ssCancelBtn) ssCancelBtn.textContent = 'Закрыть';
      if (ssPreviewHint) ssPreviewHint.textContent = 'Идёт демонстрация. Можно поменять качество или остановить.';
      // show current preview
      if (screenStream) ssSetPreview(screenStream);
      else ssSetPreview(null);
    } else {
      if (ssStartBtn) ssStartBtn.textContent = 'Начать';
      if (ssCancelBtn) ssCancelBtn.textContent = 'Отмена';
      if (ssPreviewHint) ssPreviewHint.textContent = 'Нажми «Начать», затем выбери экран/окно/вкладку.';
      ssClearPreview();
    }

    ssOverlayVisible(true);
  }

  if (btnShare) {
    btnShare.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      openScreenShareModal();
    });
  }

  // tabs
  try {
    const tabs = ssOverlay?.querySelectorAll?.('.ss-tab') || [];
    tabs.forEach((b) => {
      b.addEventListener('click', (e) => {
        e.preventDefault();
        e.stopPropagation();
        tabs.forEach((x) => x.classList.remove('active'));
        b.classList.add('active');
        ssSelectedSurface = (b.dataset.surface || 'monitor').toString();
        // if we are already sharing, don't force restart; just keep UI selection
      });
    });
  } catch (_) {}

  // quality changes while sharing
  try {
    const radios = ssOverlay?.querySelectorAll?.('input[name="ssRes"], input[name="ssFps"], #ssAudioChk') || [];
    radios.forEach((el) => {
      el.addEventListener('change', () => {
        ssApplyQualityIfSharing().catch(() => {});
      });
    });
  } catch (_) {}

  // modal close buttons
  function closeSsModal() {
    ssOverlayVisible(false);
  }

  if (ssCloseBtn) {
    ssCloseBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      closeSsModal();
    });
  }
  if (ssCancelBtn) {
    ssCancelBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      closeSsModal();
    });
  }

  // click outside modal
  if (ssOverlay) {
    ssOverlay.addEventListener('click', (e) => {
      if (e.target === ssOverlay) {
        closeSsModal();
      }
    });
  }

  if (ssStartBtn) {
    ssStartBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (isSharingScreen) {
        stopScreenShare().catch(() => {});
        closeSsModal();
      } else {
        startScreenShare().catch(() => {});
      }
    });
  }

  // remote panel controls
  if (ssPanelHideBtn) {
    ssPanelHideBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      hideRemoteViewerPanel();
    });
  }
  if (ssPanelFullscreenBtn && ssRemoteVideo) {
    ssPanelFullscreenBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      try {
        const el = ssRemoteVideo;
        if (el.requestFullscreen) el.requestFullscreen();
        else if (el.webkitRequestFullscreen) el.webkitRequestFullscreen();
      } catch (_) {}
    });
  }

  // leave voice on logout/unload
  window.addEventListener('beforeunload', () => {
    try { if (isSharingScreen) stopScreenShare().catch(() => {}); } catch (_) {}
    try { if (inChannelId) wsManager.send({ type: 'voice_leave', data: { channel_id: inChannelId } }); } catch (_) {}
  });

  // install WS voice handler (chain if already exists)
  const prev = window.onVoiceEvent;
  window.onVoiceEvent = (msg) => {
    try { if (typeof prev === 'function') prev(msg); } catch (_) {}
    onVoiceEvent(msg).catch((e) => console.warn('[VOICE] onVoiceEvent error', e));
  };

  // public API for app.js
  window.lbVoice = {
    join,
    leave,
    toggle: join,
    setMuted,
    setDeafened,
    getState: () => ({
      channel_id: inChannelId,
      channel_name: inChannelName,
      muted,
      deafened,
      peers: [...pcs.keys()]
    })
  };

  // initial state hidden
  setBarVisible(false);
  updateVoiceStageLetter();
  stageShowEmpty();
  updateStageWatchButtons();
  updateVoiceViewHeader();
}
