import { api } from "./api.js";

let friendsOpen = false;

export function initFriends() {
  console.log("[TRACE FRIENDS] initFriends() called, hash=", location.hash);
  const friendsBtn    = document.getElementById("friendsBtn");
  const chatView      = document.getElementById("chatView");
  const friendsView   = document.getElementById("friendsView");
  const channelsPanel = document.getElementById("channelsPanel");
  const membersPanel  = document.getElementById("membersPanel");

  const channelsTitle = channelsPanel?.querySelector(".panelHeader h3");
  const dmList        = channelsPanel?.querySelector(".dm-list");
  const channelsList  = document.getElementById("channels-list");
  const friendsList   = document.getElementById("friendsList");

  function show(el, display = "") {
    if (el) el.style.display = display;
  }

  function hide(el) {
    if (el) el.style.display = "none";
  }

  async function loadFriends() {
    let friends = [];
    try {
      console.log("[FRIENDS] Loading friends...");
      friends = await api("/api/friends");
      console.log("[FRIENDS] Loaded:", friends);
    } catch (err) {
      console.error("[FRIENDS] Ошибка при загрузке друзей:", err);
      friendsList.innerHTML = "<div class='error'>Ошибка загрузки друзей 😕</div>";
      return; // выходим, чтобы не продолжать
    }

    friendsList.innerHTML = "";

    if (!friends || friends.length === 0) {
      friendsList.innerHTML = "<div class='no-friends'>У вас пока нет друзей 😅</div>";
      return;
    }

    friends.forEach(f => {
      const el = document.createElement("div");
      el.className = "friend";
      el.innerHTML = `
        <div class="friend-avatar">${f.username[0]}</div>
        <div class="friend-info">
          <div class="friend-name">${f.username}</div>
          <div class="friend-activity"></div>
        </div>
      `;
      applyFriendState(el, f);
      friendsList.appendChild(el);
    });
  }

  function applyFriendState(el, f) {
    el.classList.toggle("online", f.status === "online");
    el.classList.toggle("offline", f.status !== "online");
    el.classList.toggle("pending", !!f.pending);

    const activity = el.querySelector(".friend-activity");
    if (!activity) return;

    if (f.inVoice) {
      activity.textContent = "В голосовом канале";
      activity.style.display = "";
    } else {
      activity.style.display = "none";
    }
  }

  function openFriends() {
    if (friendsOpen) return;
    friendsOpen = true;

    location.hash = "#/friends";

    hide(chatView);
    show(friendsView, "block");

    if (channelsTitle) channelsTitle.textContent = "Чаты";
    channelsPanel?.classList.add("dm-mode");

    hide(channelsList);
    show(dmList, "block");

    hide(membersPanel);
    friendsBtn?.classList.add("active");

    loadFriends().catch(console.error);
  }

  function closeFriends() {
    friendsOpen = false;

    hide(friendsView);
    show(chatView, "flex");

    if (channelsTitle) channelsTitle.textContent = "Каналы";
    channelsPanel?.classList.remove("dm-mode");

    hide(dmList);
    show(channelsList, "block");

    show(membersPanel, "flex");
    friendsBtn?.classList.remove("active");
  }

  function filterFriends(type) {
    document.querySelectorAll(".friend").forEach(el => {
      const online  = el.classList.contains("online");
      const pending = el.classList.contains("pending");

      if (type === "all") el.style.display = "";
      else if (type === "online") el.style.display = online ? "" : "none";
      else if (type === "pending") el.style.display = pending ? "" : "none";
    });
  }

  friendsBtn?.addEventListener("click", openFriends);

  document.querySelectorAll(".friends-tabs .tab").forEach(tab => {
    tab.addEventListener("click", () => {
      document.querySelectorAll(".friends-tabs .tab")
        .forEach(t => t.classList.remove("active"));

      tab.classList.add("active");
      filterFriends(tab.dataset.filter);
    });
  });

  const addFriendBtn = document.getElementById("addFriendBtn");
  if (addFriendBtn) {
    addFriendBtn.onclick = async () => {
      const userId = window.getSelectedUserId?.();
      if (!userId) return;

      await api("/api/friends/add", "POST", { userId });
      loadFriends();
    };
  }

  // если был прямой заход по #/friends
  if (location.hash === "#/friends") {
    openFriends();
  }

  // экспорт в window (если нужно из других мест)
  window.openFriends  = openFriends;
  window.closeFriends = closeFriends;
}
