(() => {
  const revealItems = Array.from(document.querySelectorAll('[data-reveal]'));

  if (!revealItems.length) {
    return;
  }

  if (!('IntersectionObserver' in window)) {
    revealItems.forEach((item) => item.classList.add('is-visible'));
    return;
  }

  const observer = new IntersectionObserver((entries) => {
    entries.forEach((entry) => {
      if (!entry.isIntersecting) return;
      entry.target.classList.add('is-visible');
      observer.unobserve(entry.target);
    });
  }, { threshold: 0.18 });

  revealItems.forEach((item) => observer.observe(item));
})();
