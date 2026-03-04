import { api } from "./api.js?v=7";
import { showUserMenu } from "./user-menu.js?v=7";

let friendsOpen = false;

function isFriendsHash() {
  return location.hash === "#/friends" || location.hash === "#friends";
}

function statusToLabel(status) {
  const s = (status || "offline").toString().toLowerCase();
  if (s === "online") return "В сети";
  if (s === "idle") return "Не активен";
  if (s === "dnd") return "Не беспокоить";
  return "Не в сети";
}

export function initFriends() {
  console.log("[FRIENDS] initFriends()");

  const friendsBtn = document.getElementById("friendsBtn");
  const chatView = document.getElementById("chatView");
  const friendsView = document.getElementById("friendsView");
  const channelsPanel = document.getElementById("channelsPanel");
  const membersPanel = document.getElementById("membersPanel");

  const channelsTitle = channelsPanel?.querySelector(".panelHeader h3");
  const dmList = document.getElementById("dmList");
  const channelsList = document.getElementById("channels-list");

  const friendsList = document.getElementById("friendsList");
  const addFriendBtn = document.getElementById("addFriendBtn");
  const findFriendsBtn = document.getElementById("findFriendsBtn");

  function setHidden(el, hidden) {
    if (!el) return;
    if ("hidden" in el) el.hidden = hidden;
    else el.style.display = hidden ? "none" : "";
  }

  function ensureFriendsLayout() {
    if (!friendsList) return null;

    let panel = document.getElementById("friendsSearchPanel");
    let items = document.getElementById("friendsItems");

    if (!panel || !items) {
      friendsList.innerHTML = `
        <div class="friends-search-panel" id="friendsSearchPanel" hidden>
          <div class="friends-search-row">
            <input class="inp" id="friendsSearchInput" placeholder="Найти пользователя по нику..." autocomplete="off" />
            <button class="btn btn-secondary" id="friendsSearchBtn" type="button">Найти</button>
          </div>
          <div class="friends-search-results" id="friendsSearchResults"></div>
        </div>
        <div class="friends-items" id="friendsItems"></div>
      `;

      panel = document.getElementById("friendsSearchPanel");
      items = document.getElementById("friendsItems");
    }

    return { panel, items };
  }

  function closeSearch() {
    const layout = ensureFriendsLayout();

    const panel = document.getElementById("friendsSearchPanel");
    if (panel) setHidden(panel, true);

    if (layout?.items) setHidden(layout.items, false);

    const input = document.getElementById("friendsSearchInput");
    if (input) input.value = "";

    const results = document.getElementById("friendsSearchResults");
    if (results) results.innerHTML = "";
  }

  async function loadPending() {
    const layout = ensureFriendsLayout();
    if (!layout) return;
    const { items } = layout;

    items.innerHTML = `<div class="muted" style="padding:12px;">Загрузка заявок...</div>`;

    let incoming = [];
    let outgoing = [];
    try {
      [incoming, outgoing] = await Promise.all([
        api("/api/friends/requests/incoming"),
        api("/api/friends/requests/outgoing"),
      ]);
    } catch (err) {
      console.error("[FRIENDS] pending load error:", err);
      items.innerHTML = `<div class="error">Ошибка загрузки заявок 😕</div>`;
      return;
    }

    const ids = new Set();
    for (const r of incoming || []) ids.add(r.sender_id);
    for (const r of outgoing || []) ids.add(r.receiver_id);

    const users = new Map();
    await Promise.all(
      [...ids].map(async (id) => {
        try {
          const u = await api(`/api/users/${id}`);
          users.set(id, u);
        } catch (_) {
          users.set(id, { id, username: `User#${id}` });
        }
      })
    );

    items.innerHTML = "";

    if ((!incoming || incoming.length === 0) && (!outgoing || outgoing.length === 0)) {
      items.innerHTML = `<div class="muted" style="padding:12px;">Нет заявок</div>`;
      return;
    }

    const mkHeader = (t) => {
      const h = document.createElement("div");
      h.className = "muted";
      h.style.padding = "12px 12px 6px";
      h.style.fontWeight = "600";
      h.textContent = t;
      return h;
    };

    const mkRow = ({ user, rightHtml }) => {
      const row = document.createElement("div");
      row.className = "friend-search-item";
      const letter = (user?.username || "U").charAt(0).toUpperCase();
      row.innerHTML = `
        <div class="avatar small">${letter}</div>
        <div class="text">
          <div class="name">${user?.username || "Unknown"}</div>
          <div class="role">ID: ${user?.id ?? "?"}</div>
        </div>
        ${rightHtml || ""}
      `;
      return row;
    };

    if (incoming && incoming.length) {
      items.appendChild(mkHeader("Входящие"));
      for (const r of incoming) {
        const user = users.get(r.sender_id) || { id: r.sender_id, username: `User#${r.sender_id}` };

        const row = mkRow({
          user,
          rightHtml: `
            <div style="display:flex; gap:8px;">
              <button class="btn btn-secondary btn-small" type="button" data-act="accept">Принять</button>
              <button class="btn btn-ghost btn-small" type="button" data-act="decline">Отклонить</button>
            </div>
          `,
        });

        row.querySelector('[data-act="accept"]')?.addEventListener("click", async (e) => {
          e.stopPropagation();
          try {
            await api(`/api/friends/accept/${r.id}`, { method: "POST" });
            await loadPending();
            window.dispatchEvent(new CustomEvent("laberry:friends-refresh"));
          } catch (err) {
            console.error("[FRIENDS] accept error", err);
          }
        });

        row.querySelector('[data-act="decline"]')?.addEventListener("click", async (e) => {
          e.stopPropagation();
          try {
            await api(`/api/friends/decline/${r.id}`, { method: "POST" });
            await loadPending();
            window.dispatchEvent(new CustomEvent("laberry:friends-refresh"));
          } catch (err) {
            console.error("[FRIENDS] decline error", err);
          }
        });

        row.querySelector(".avatar")?.addEventListener("click", (e) => {
          e.stopPropagation();
          const uid = Number(user?.id);
          if (!Number.isFinite(uid) || uid <= 0) return;
          showUserMenu({
            userId: uid,
            username: (user?.username || "Unknown").toString(),
            anchorEl: e.currentTarget,
            allowDm: true,
            allowAddFriend: false,
            allowRemoveFriend: false,
          });
        });

        items.appendChild(row);
      }
    }

    if (outgoing && outgoing.length) {
      items.appendChild(mkHeader("Отправленные"));
      for (const r of outgoing) {
        const user = users.get(r.receiver_id) || { id: r.receiver_id, username: `User#${r.receiver_id}` };
        const row = mkRow({
          user,
          rightHtml: `
            <div style="display:flex; gap:8px; align-items:center;">
              <div class="muted">Отправлено</div>
              <button class="btn btn-ghost btn-small" type="button" data-act="cancel">Отменить</button>
            </div>
          `,
        });

        row.querySelector('[data-act="cancel"]')?.addEventListener("click", async (e) => {
          e.stopPropagation();
          try {
            await api(`/api/friends/cancel/${r.id}`, { method: "POST" });
            await loadPending();
            window.dispatchEvent(new CustomEvent("laberry:friends-refresh"));
          } catch (err) {
            console.error("[FRIENDS] cancel error", err);
          }
        });

        row.querySelector(".avatar")?.addEventListener("click", (e) => {
          e.stopPropagation();
          const uid = Number(user?.id);
          if (!Number.isFinite(uid) || uid <= 0) return;
          showUserMenu({
            userId: uid,
            username: (user?.username || "Unknown").toString(),
            anchorEl: e.currentTarget,
            allowDm: true,
            allowAddFriend: false,
            allowRemoveFriend: false,
          });
        });

        items.appendChild(row);
      }
    }
  }

  function openFriends(opts = {}) {
    if (friendsOpen) return;
    friendsOpen = true;

    try { window.closeVoiceView?.(); } catch (_) {}
    try {
      const voiceView = document.getElementById('voiceView');
      setHidden(voiceView, true);
      document.body.classList.remove('voice-view-open');
    } catch (_) {}

    closeSearch();

    if (opts.setHash !== false) {
      location.hash = "#/friends";
    }

    setHidden(chatView, true);
    setHidden(friendsView, false);

    const friendsScroll = document.getElementById("friendsList") || document.getElementById("friendsItems") || friendsView;
    requestAnimationFrame(() => {
      try {
        if (friendsScroll) friendsScroll.scrollTop = 0;
      } catch (_) {}
    });

    if (channelsTitle) channelsTitle.textContent = "Чаты";
    channelsPanel?.classList.add("dm-mode");

    setHidden(channelsList, true);
    setHidden(dmList, false);

    try {
      window.lbLoadDmList?.();
    } catch (_) {}

    setHidden(membersPanel, true);
    friendsBtn?.classList.add("active");

    const active = document.querySelector(".friends-tabs .tab.active")?.dataset?.filter || "online";
    if (active === "pending") loadPending().catch(console.error);
    else loadFriends().then(() => applyFriendsFilter(active)).catch(console.error);
  }

  function closeFriends(opts = {}) {
    friendsOpen = false;

    closeSearch();

    setHidden(friendsView, true);
    setHidden(chatView, false);

    if (channelsTitle) channelsTitle.textContent = "Каналы";
    channelsPanel?.classList.remove("dm-mode");

    setHidden(dmList, true);
    setHidden(channelsList, false);

    setHidden(membersPanel, false);
    friendsBtn?.classList.remove("active");

    if (isFriendsHash() && opts.clearHash !== false) {
      try {
        history.replaceState(null, "", location.pathname + location.search);
      } catch (_) {
        location.hash = "";
      }
    }
  }

  async function loadFriends() {
    const layout = ensureFriendsLayout();
    if (!layout) return;

    const { items } = layout;
    items.innerHTML = `<div class="muted" style="padding:12px;">Загрузка...</div>`;

    let friends = [];
    try {
      friends = await api("/api/friends");
    } catch (err) {
      console.error("[FRIENDS] load error:", err);
      items.innerHTML = `<div class="error">Ошибка загрузки друзей 😕</div>`;
      return;
    }

    items.innerHTML = "";

    if (!friends || friends.length === 0) {
      items.innerHTML = `
        <div class="no-friends">
          <p>У вас пока нет друзей 😅</p>
          <button class="btn btn-secondary" id="findFriendsBtnInline" type="button">Найти друзей</button>
        </div>
      `;
      const btn = document.getElementById("findFriendsBtnInline");
      btn?.addEventListener("click", openSearch);
      return;
    }

    for (const f of friends) {
      const el = document.createElement("div");
      let st = (f.status || (f.is_online ? "online" : "offline")).toString().toLowerCase();
      if (st === "invisible") st = "offline";
      const online = st !== "offline";
      el.className = `member friend status-${st} ${online ? "online" : ""}`;
      const letter = (f.username || "U").charAt(0).toUpperCase();
      el.innerHTML = `
        <div class="avatar small">${letter}</div>
        <div class="text">
          <div class="name">${f.username || "Unknown"}</div>
          <div class="role">${statusToLabel(st)}</div>
        </div>
      `;
      el.dataset.userId = String(f.id);
      el.dataset.username = (f.username || "").toString();

      el.addEventListener("click", (e) => {
        const uid = Number(f?.id);
        if (!Number.isFinite(uid) || uid <= 0) return;

        const anchor = e?.target?.closest?.(".avatar") || el;

        e.stopPropagation();
        showUserMenu({
          userId: uid,
          username: (f.username || "Unknown").toString(),
          anchorEl: anchor,
          allowDm: true,
          allowAddFriend: false,
          allowRemoveFriend: true,
        });
      });

      items.appendChild(el);
    }
  }

  function applyFriendsFilter(type) {
    document.querySelectorAll("#friendsItems .friend").forEach((el) => {
      const online = el.classList.contains("online");
      if (type === "all") el.style.display = "";
      else if (type === "online") el.style.display = online ? "" : "none";
      else el.style.display = "none";
    });
  }

  function openSearch() {
    const layout = ensureFriendsLayout();
    if (!layout) return;
    const { panel, items } = layout;

    if (panel && panel.hidden === false) {
      closeSearch();
      return;
    }

    setHidden(panel, false);
    setHidden(items, true);

    const input = document.getElementById("friendsSearchInput");
    const btn = document.getElementById("friendsSearchBtn");
    const results = document.getElementById("friendsSearchResults");

    const doSearch = async () => {
      const q = (input?.value || "").trim();
      if (!q) return;

      results.innerHTML = `<div class="muted" style="padding:10px;">Поиск...</div>`;

      let users = [];
      try {
        users = await api(`/api/users/search?query=${encodeURIComponent(q)}`);
      } catch (err) {
        console.error("[FRIENDS] search error", err);
        results.innerHTML = `<div class="error">Ошибка поиска 😕</div>`;
        return;
      }

      if (!users || users.length === 0) {
        results.innerHTML = `<div class="muted" style="padding:10px;">Никого не найдено</div>`;
        return;
      }

      results.innerHTML = "";
      for (const u of users) {
        const row = document.createElement("div");
        row.className = "friend-search-item";

        const letter = (u.username || "U").charAt(0).toUpperCase();
        row.innerHTML = `
          <div class="avatar small">${letter}</div>
          <div class="text">
            <div class="name">${u.username || "Unknown"}</div>
            <div class="role">ID: ${u.id}</div>
          </div>
          <button class="btn btn-ghost" type="button">Добавить</button>
        `;

        const showMenu = (anchorEl) => {
          const uid = Number(u?.id);
          if (!Number.isFinite(uid) || uid <= 0) return;
          showUserMenu({
            userId: uid,
            username: (u.username || "Unknown").toString(),
            anchorEl,
            allowDm: true,
            allowAddFriend: true,
            allowRemoveFriend: false,
          });
        };

        row.querySelector(".avatar")?.addEventListener("click", (e) => {
          e.stopPropagation();
          showMenu(e.currentTarget);
        });

        row.querySelector(".name")?.addEventListener("click", (e) => {
          e.stopPropagation();
          showMenu(row.querySelector(".avatar") || row);
        });

        const addBtn = row.querySelector("button");
        addBtn?.addEventListener("click", async (e) => {
          e.stopPropagation();
          addBtn.disabled = true;
          addBtn.textContent = "…";

          try {
            await api("/api/friends/request", {
              method: "POST",
              body: JSON.stringify({ receiver_id: u.id }),
            });
            addBtn.textContent = "Отправлено";
            window.dispatchEvent(new CustomEvent("laberry:friends-refresh"));
          } catch (err) {
            console.error("[FRIENDS] request error", err);
            addBtn.disabled = false;
            addBtn.textContent = "Ошибка";
            setTimeout(() => (addBtn.textContent = "Добавить"), 1200);
          }
        });

        results.appendChild(row);
      }
    };

    if (btn) btn.onclick = doSearch;
    if (input) {
      input.onkeydown = (e) => {
        if (e.key === "Enter") doSearch();
      };
    }

    input?.focus();
  }

  friendsBtn?.addEventListener("click", () => {
    if (friendsOpen) closeFriends();
    else openFriends();
  });

  addFriendBtn?.addEventListener("click", openSearch);
  findFriendsBtn?.addEventListener("click", openSearch);

  document.querySelectorAll(".friends-tabs .tab").forEach((tab) => {
    const type = tab.dataset.filter;
    if (!type) return;

    tab.addEventListener("click", async () => {
      document
        .querySelectorAll(".friends-tabs .tab[data-filter]")
        .forEach((t) => t.classList.remove("active"));
      tab.classList.add("active");

      closeSearch();

      if (type === "pending") {
        await loadPending();
        return;
      }

      await loadFriends();
      applyFriendsFilter(type);
    });
  });

  const syncFromHash = () => {
    if (isFriendsHash() && !friendsOpen) openFriends({ setHash: false });
    if (!isFriendsHash() && friendsOpen) closeFriends({ clearHash: false });
  };

  window.addEventListener("hashchange", syncFromHash);
  syncFromHash();

  window.addEventListener("laberry:friends-refresh", () => {
    if (!friendsOpen) return;

    const active = document.querySelector(".friends-tabs .tab.active")?.dataset?.filter || "online";
    closeSearch();

    if (active === "pending") {
      loadPending().catch(console.error);
    } else {
      loadFriends()
        .then(() => applyFriendsFilter(active))
        .catch(console.error);
    }
  });

  window.addEventListener("laberry:friends-force-exit", () => {
    if (!friendsOpen) return;
    friendsOpen = false;
    friendsBtn.classList.remove("active");
  });
  window.openFriends = openFriends;
  window.closeFriends = closeFriends;
}
