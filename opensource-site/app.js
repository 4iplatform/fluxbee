const root = document.documentElement;
const languageButtons = document.querySelectorAll("[data-set-lang]");

function setLanguage(language) {
  const lang = language === "en" ? "en" : "es";
  root.dataset.lang = lang;
  root.lang = lang;
  document.querySelectorAll("[data-es][data-en]").forEach((element) => {
    element.textContent = element.dataset[lang];
  });
  languageButtons.forEach((button) => {
    button.setAttribute("aria-pressed", String(button.dataset.setLang === lang));
  });
  document.title = lang === "es"
    ? "Fluxbee Open Source — Sistemas de IA que trabajan como uno"
    : "Fluxbee Open Source — AI systems that work as one";
  try { localStorage.setItem("fluxbee-oss-lang", lang); } catch (_) {}
}

languageButtons.forEach((button) => {
  button.addEventListener("click", () => setLanguage(button.dataset.setLang));
});

const preferredLanguage = (() => {
  try {
    const saved = localStorage.getItem("fluxbee-oss-lang");
    if (saved) return saved;
  } catch (_) {}
  return navigator.language.toLowerCase().startsWith("es") ? "es" : "en";
})();
setLanguage(preferredLanguage);

const menuButton = document.querySelector(".menu-button");
const mobileMenu = document.querySelector(".mobile-menu");
menuButton.addEventListener("click", () => {
  const open = mobileMenu.classList.toggle("open");
  menuButton.setAttribute("aria-expanded", String(open));
});
mobileMenu.querySelectorAll("a").forEach((link) => {
  link.addEventListener("click", () => {
    mobileMenu.classList.remove("open");
    menuButton.setAttribute("aria-expanded", "false");
  });
});

document.querySelectorAll(".copy-button").forEach((button) => {
  button.addEventListener("click", async () => {
    const target = document.getElementById(button.dataset.copyTarget);
    if (!target) return;
    try {
      await navigator.clipboard.writeText(target.innerText);
      const label = button.querySelector("span");
      const originalEs = label.dataset.es;
      const originalEn = label.dataset.en;
      label.textContent = root.dataset.lang === "es" ? "Copiado" : "Copied";
      window.setTimeout(() => {
        label.dataset.es = originalEs;
        label.dataset.en = originalEn;
        label.textContent = root.dataset.lang === "es" ? originalEs : originalEn;
      }, 1600);
    } catch (_) {}
  });
});

const revealObserver = new IntersectionObserver((entries) => {
  entries.forEach((entry) => {
    if (entry.isIntersecting) {
      entry.target.classList.add("visible");
      revealObserver.unobserve(entry.target);
    }
  });
}, { threshold: .1, rootMargin: "0px 0px -40px" });
document.querySelectorAll(".reveal").forEach((element) => revealObserver.observe(element));
