const token = localStorage.getItem("auth_token");

if (token) {
  // Проверяем действителен ли токен
  fetch("/api/auth/verify", {
    headers: { Authorization: `Bearer ${token}` },
  })
    .then((res) => {
      if (res.ok) {
        // токен валиден → сразу заходим в /app
        window.location.href = "/app";
      } else {
        // токен устарел → очищаем
        localStorage.removeItem("auth_token");
      }
    })
    .catch(() => {
      localStorage.removeItem("auth_token");
    });
}
