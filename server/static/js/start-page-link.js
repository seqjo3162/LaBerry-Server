(() => {
  const START_PAGE_URL = "/start";

  function createModal() {
    let overlay = document.getElementById("startPagePromptOverlay");
    if (overlay) return overlay;

    overlay = document.createElement("div");
    overlay.id = "startPagePromptOverlay";
    overlay.className = "start-page-prompt-overlay";
    overlay.hidden = true;
    overlay.innerHTML = `
      <div class="start-page-prompt" role="dialog" aria-modal="true" aria-labelledby="startPagePromptTitle">
        <button class="start-page-prompt-close" type="button" aria-label="Закрыть" data-start-page-close>✕</button>
        <div class="start-page-prompt-badge">LaBerry</div>
        <h2 id="startPagePromptTitle">Хочешь увидеть начальную страницу LaBerry?</h2>
        <p>Откроется отдельная стартовая страница проекта. Текущий вход и мессенджер останутся без изменений.</p>
        <div class="start-page-prompt-actions">
          <button class="btn btn-primary" type="button" data-start-page-go>Перейти</button>
          <button class="btn btn-ghost" type="button" data-start-page-close>Остаться</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    overlay.addEventListener("click", (event) => {
      if (event.target === overlay || event.target.closest("[data-start-page-close]")) {
        closeModal();
      }
      if (event.target.closest("[data-start-page-go]")) {
        window.location.href = START_PAGE_URL;
      }
    });

    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && !overlay.hidden) closeModal();
    });

    return overlay;
  }

  function openModal() {
    const overlay = createModal();
    overlay.hidden = false;
    document.body.classList.add("start-page-prompt-open");
    overlay.querySelector("[data-start-page-go]")?.focus?.();
  }

  function closeModal() {
    const overlay = document.getElementById("startPagePromptOverlay");
    if (!overlay) return;
    overlay.hidden = true;
    document.body.classList.remove("start-page-prompt-open");
  }

  function wireTrigger(trigger) {
    if (!trigger || trigger.dataset.startPageTriggerWired === "1") return;
    trigger.dataset.startPageTriggerWired = "1";
    trigger.setAttribute("role", trigger.getAttribute("role") || "button");
    trigger.setAttribute("tabindex", trigger.getAttribute("tabindex") || "0");
    trigger.setAttribute("title", trigger.getAttribute("title") || "Начальная страница LaBerry");

    trigger.addEventListener("click", (event) => {
      event.preventDefault();
      openModal();
    });

    trigger.addEventListener("keydown", (event) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      openModal();
    });
  }

  function init() {
    document.querySelectorAll("[data-start-page-trigger]").forEach(wireTrigger);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init, { once: true });
  } else {
    init();
  }
})();
