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
  const existingToken = localStorage.getItem("auth_token"); // <-- ИЗМЕНЕНО: "token" → "auth_token"
  if (existingToken) {
    try {
      const verify = await fetch("/api/auth/verify", {
        headers: { Authorization: `Bearer ${existingToken}` },
      });
      if (verify.ok) {
        console.log("✅ Token verified, redirecting to /app");
        window.location.href = "/app";
        return;
      } else {
        console.warn("Token invalid, clearing localStorage");
        localStorage.removeItem("auth_token"); // <-- ИЗМЕНЕНО
        localStorage.removeItem("user_id");
      }
    } catch (err) {
      console.warn("Verification failed:", err);
    }
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
      showError("✅ Аккаунт создан. Теперь войдите.");
      setMode("login");
      return;
    }

    // LOGIN SUCCESS
    localStorage.setItem("auth_token", data.access_token); // <-- ИЗМЕНЕНО: "token" → "auth_token"
    localStorage.setItem("user_id", data.user_id);
    window.location.href = "/app";
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