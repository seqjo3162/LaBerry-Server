(function () {
  const FORM_ID = 'admin-login-form';
  const form = document.getElementById(FORM_ID);
  if (!form) return;

  form.addEventListener('submit', async function (event) {
    event.preventDefault();
    const submit = form.querySelector('button[type=submit]');
    if (submit) submit.disabled = true;

    const data = new FormData(form);
    const response = await fetch(form.action, {
      method: 'POST',
      body: data,
      redirect: 'manual',
      credentials: 'same-origin',
    });

    const location = response.headers.get('location');
    if (location) {
      window.location.href = location;
      return;
    }

    window.location.reload();
  });
})();
