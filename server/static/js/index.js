const token = localStorage.getItem("auth_token");

if (token) {
  fetch("/api/users/me", {
    headers: { Authorization: `Bearer ${token}` },
  })
    .then((res) => {
      if (res.ok) window.location.href = "/app";
      else localStorage.removeItem("auth_token");
    })
    .catch(() => localStorage.removeItem("auth_token"));
}
