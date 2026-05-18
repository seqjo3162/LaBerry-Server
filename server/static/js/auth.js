async function tryRestoreSession() {
  const authToken = localStorage.getItem("auth_token");
  if (authToken) return authToken;

  const refreshToken = localStorage.getItem("refresh_token");
  if (!refreshToken) return null;

  try {
    const refresh = await fetch("/api/auth/refresh", {
      method: "POST",
      headers: { Authorization: `Bearer ${refreshToken}` },
    });

    if (!refresh.ok) return null;

    const data = await refresh.json().catch(() => null);
    if (data?.access_token) {
      localStorage.setItem("auth_token", data.access_token);
    }
    if (data?.refresh_token) {
      localStorage.setItem("refresh_token", data.refresh_token);
    }
    return data?.access_token || null;
  } catch {
    return null;
  }
}

document.addEventListener("DOMContentLoaded", async () => {
  const form = document.getElementById("auth-form");
  const errorBox = document.getElementById("error");
  const submitBtn = document.getElementById("submit-btn");
  const toRegister = document.getElementById("to-register");
  const toLogin = document.getElementById("to-login");
  const switchText = document.getElementById("switch-text");

  if (!form || !errorBox || !submitBtn) {
    console.error("auth elements not found");
    return;
  }

  // ==============================
  // 🧠 AUTO-LOGIN CHECK
  // ==============================
  const restoredToken = await tryRestoreSession();
  if (restoredToken) {
    console.log("✅ Session restored, redirecting to /app");
    window.location.href = "/app";
    return;
  }

  if (localStorage.getItem("refresh_token")) {
    console.warn("Token invalid, clearing localStorage");
    localStorage.removeItem("auth_token");
    localStorage.removeItem("refresh_token");
    localStorage.removeItem("user_id");
  }

  // ==============================
  // MODE SWITCH
  // ==============================
  let mode = "login";

  function showError(msg) {
    errorBox.textContent = msg;
    errorBox.hidden = false;
  }

  function clearError() {
    errorBox.textContent = "";
    errorBox.hidden = true;
  }

  function mapError(code) {
    const m = {
      invalid_credentials: "Неверный логин или пароль",
      user_exists: "Пользователь уже существует",
      weak_password: "Пароль слишком короткий",
      bad_request: "Неверные данные",
    };
    return m[code] || "Ошибка авторизации";
  }

  function saveSessionAndOpenApp(data) {
    localStorage.setItem("auth_token", data.access_token);
    if (data?.refresh_token) {
      localStorage.setItem("refresh_token", data.refresh_token);
    } else {
      localStorage.removeItem("refresh_token");
    }
    localStorage.setItem("user_id", data.user_id);
    window.location.href = "/app";
  }

  async function loginWithCredentials(username, password) {
    const body = new URLSearchParams();
    body.append("username", username);
    body.append("password", password);

    const loginRes = await fetch("/api/auth/login", {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body,
    });

    const loginData = await loginRes.json().catch(() => null);
    if (!loginRes.ok || !loginData?.access_token) {
      throw new Error(loginData?.error || "auto_login_failed");
    }

    return loginData;
  }

  // ==============================
  // FORM SUBMIT
  // ==============================
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    clearError();

    const username = form.username.value.trim();
    const password = form.password.value;

    if (!username || !password) {
      showError("Заполните все поля");
      return;
    }

    let res, data;
    try {
      if (mode === "login") {
        // LOGIN → x-www-form-urlencoded
        const body = new URLSearchParams();
        body.append("username", username);
        body.append("password", password);

        res = await fetch("/api/auth/login", {
          method: "POST",
          headers: { "Content-Type": "application/x-www-form-urlencoded" },
          body,
        });
      } else {
        // REGISTER → JSON
        res = await fetch("/api/auth/register", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ username, password }),
        });
      }

      data = await res.json();
    } catch {
      showError("Сервер недоступен");
      return;
    }

    if (!res.ok) {
      showError(mapError(data.error));
      return;
    }

    if (mode === "register") {
      try {
        const loginData = await loginWithCredentials(username, password);
        saveSessionAndOpenApp(loginData);
      } catch {
        showError("Аккаунт создан, но автоматический вход не сработал. Войдите вручную.");
        setMode("login");
      }
      return;
    }

    // LOGIN SUCCESS
    saveSessionAndOpenApp(data);
  });

  // ==============================
  // MODE SWITCH HANDLING
  // ==============================
  function setMode(next) {
    mode = next;
    clearError();

    const title = document.getElementById("title");

    if (mode === "register") {
      submitBtn.textContent = "Зарегистрироваться";
      if (toRegister) toRegister.hidden = true;
      if (toLogin) toLogin.hidden = false;
      if (switchText) switchText.textContent = "Уже есть аккаунт?";
      if (title) title.textContent = "Регистрация";
      form.username.placeholder = "Username";
      form.password.placeholder = "Password (min 8 символов)";
    } else {
      submitBtn.textContent = "Войти";
      if (toRegister) toRegister.hidden = false;
      if (toLogin) toLogin.hidden = true;
      if (switchText) switchText.textContent = "Нет аккаунта?";
      if (title) title.textContent = "LaBerry";
      form.username.placeholder = "Username";
      form.password.placeholder = "Password";
    }
  }

  if (toRegister)
    toRegister.addEventListener("click", (e) => {
      e.preventDefault();
      setMode("register");
    });

  if (toLogin)
    toLogin.addEventListener("click", (e) => {
      e.preventDefault();
      setMode("login");
    });
});
