import { api } from "./api.js?v=10";

let backdropEl = null;
let menuEl = null;
let lastOpenTs = 0;

function ensureMenu() {
  if (backdropEl && menuEl) return;

  backdropEl = document.createElement("div");
  backdropEl.className = "lb-menu-backdrop";
  backdropEl.hidden = true;

  menuEl = document.createElement("div");
  menuEl.className = "lb-user-menu";
  menuEl.hidden = true;

  backdropEl.addEventListener("click", (e) => {
    const now = Date.now();
    if (now - lastOpenTs < 250) {
      e.preventDefault();
      e.stopPropagation();
      return;
    }
    hideUserMenu();
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") hideUserMenu();
  });

  document.body.appendChild(backdropEl);
  document.body.appendChild(menuEl);
}

function toast(text) {
  try {
    const el = document.createElement("div");
    el.className = "lb-toast";
    el.textContent = text;
    document.body.appendChild(el);
    setTimeout(() => el.remove(), 1600);
  } catch (_) {}
}

function hideUserMenu() {
  if (backdropEl) backdropEl.hidden = true;
  if (menuEl) menuEl.hidden = true;
}

function clamp(n, a, b) {
  return Math.max(a, Math.min(b, n));
}

function placeMenuAt(x, y) {
  if (!menuEl) return;

  // show first to measure
  menuEl.style.left = "0px";
  menuEl.style.top = "0px";
  menuEl.hidden = false;

  const pad = 8;
  const w = menuEl.offsetWidth || 240;
  const h = menuEl.offsetHeight || 180;

  const maxX = window.innerWidth - w - pad;
  const maxY = window.innerHeight - h - pad;

  const px = clamp(x, pad, Math.max(pad, maxX));
  const py = clamp(y, pad, Math.max(pad, maxY));

  menuEl.style.left = px + "px";
  menuEl.style.top = py + "px";
}

export function showUserMenu(opts) {
  ensureMenu();

  const userId = Number(opts?.userId);
  const username = (opts?.username || "Unknown").toString();
  const anchorEl = opts?.anchorEl || null;

  const allowDm = opts?.allowDm !== false;
  const allowAddFriend = opts?.allowAddFriend !== false;
  const allowRemoveFriend = !!opts?.allowRemoveFriend;

  if (!Number.isFinite(userId) || userId <= 0) return;

  const items = [];

  if (allowDm) {
    items.push({ key: "dm", label: "✉ Написать" });
  }
  if (allowAddFriend) {
    items.push({ key: "add", label: "➕ Добавить в друзья" });
  }
  if (allowRemoveFriend) {
    items.push({ key: "remove", label: "🗑 Удалить из друзей", danger: true });
  }

  items.push({ key: "profile", label: "👤 Профиль" });

  items.push({ key: "sep" });
  items.push({ key: "copy", label: "📋 Копировать ID" });

  menuEl.innerHTML = `
    <div class="lb-menu-title">${escapeHtml(username)}</div>
    ${items
      .map((it) => {
        if (it.key === "sep") return `<div class="lb-menu-sep"></div>`;
        const cls = ["lb-menu-item", it.danger ? "danger" : ""].filter(Boolean).join(" ");
        return `<button type="button" class="${cls}" data-act="${it.key}">${escapeHtml(it.label)}</button>`;
      })
      .join("")}
  `;

  menuEl.querySelectorAll("[data-act]").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      e.preventDefault();
      e.stopPropagation();

      const act = btn.getAttribute("data-act");

      try {
        if (act === "copy") {
          await navigator.clipboard.writeText(String(userId));
          toast("ID скопирован");
          window.dispatchEvent(new CustomEvent("laberry:friends-refresh"));
          hideUserMenu();
          return;
        }

        if (act === "add") {
          const res = await api("/api/friends/request", {
            method: "POST",
            body: JSON.stringify({ receiver_id: userId }),
          });

          if (res?.already_friends) toast("Уже в друзьях");
          else if (res?.incoming_pending) toast("У вас уже есть входящая заявка");
          else if (res?.dedup) toast("Заявка уже отправлена");
          else toast("Заявка отправлена");

          hideUserMenu();
          return;
        }

        if (act === "remove") {
          await api(`/api/friends/${userId}`, { method: "DELETE" });
          toast("Удалено из друзей");
          window.dispatchEvent(new CustomEvent("laberry:friends-refresh"));
          hideUserMenu();
          return;
        }

        if (act === "dm") {
          const r = await api(`/api/dms/with/${userId}`, { method: "POST" });
          const chatId = Number(r?.chat_id);
          if (Number.isFinite(chatId) && chatId > 0) {
            window.dispatchEvent(
              new CustomEvent("laberry:dm-open", { detail: { chatId, username } })
            );
            hideUserMenu();
            return;
          }
          toast("Не удалось открыть диалог");
          hideUserMenu();
          return;
        }

        if (act === "profile") {
          window.dispatchEvent(new CustomEvent("laberry:profile-open", { detail: { userId, username } }));
          hideUserMenu();
          return;
        }
      } catch (err) {
        console.warn("[UI] user menu action failed", act, err);
        toast("Ошибка");
        hideUserMenu();
      }
    });
  });

  let x = Number(opts?.x);
  let y = Number(opts?.y);

  if ((!Number.isFinite(x) || !Number.isFinite(y)) && anchorEl && anchorEl.getBoundingClientRect) {
    const r = anchorEl.getBoundingClientRect();
    x = r.right + 8;
    y = r.top;
  }

  if (!Number.isFinite(x) || !Number.isFinite(y)) {
    x = window.innerWidth / 2;
    y = window.innerHeight / 2;
  }

  lastOpenTs = Date.now();
  backdropEl.hidden = false;
  placeMenuAt(x, y);
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#039;");
}
