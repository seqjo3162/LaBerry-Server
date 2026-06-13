(function () {
  const FORM_ID = 'admin-login-form';
  const form = document.getElementById(FORM_ID);
  if (!form) return;

  form.addEventListener('submit', async function (event) {
    event.preventDefault();

    const submit = form.querySelector('button[type=submit]');
    if (submit) submit.disabled = true;

    try {
      const passwordInput = form.querySelector('input[name="password"]');
      const payload = { password: passwordInput ? passwordInput.value : '' };

      const response = await fetch(form.action, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
        credentials: 'same-origin',
      });

      if (response.url) {
        window.location.href = response.url;
      } else {
        window.location.reload();
      }
    } catch (error) {
      console.error('Ошибка сети:', error);
      if (submit) submit.disabled = false;
    }
  });
})();