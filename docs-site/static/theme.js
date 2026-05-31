// Dark/light theme toggle and mobile nav toggle

(function () {
  'use strict';

  // --- Theme ---
  function getPreferredTheme() {
    const stored = localStorage.getItem('theme');
    if (stored) return stored;
    if (window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches) {
      return 'dark';
    }
    return 'light';
  }

  function applyTheme(theme) {
    document.documentElement.setAttribute('data-theme', theme);
    localStorage.setItem('theme', theme);
    const btn = document.getElementById('theme-toggle');
    if (btn) {
      btn.textContent = theme === 'dark' ? '☀' : '☾';
      btn.setAttribute('aria-label', theme === 'dark' ? 'Switch to light mode' : 'Switch to dark mode');
    }
  }

  // Apply theme immediately (before DOMContentLoaded) to avoid flash
  applyTheme(getPreferredTheme());

  function setupThemeToggle() {
    const btn = document.getElementById('theme-toggle');
    if (!btn) return;
    btn.addEventListener('click', function () {
      const current = document.documentElement.getAttribute('data-theme') || 'light';
      applyTheme(current === 'dark' ? 'light' : 'dark');
    });
  }

  // --- Mobile nav toggle ---
  function setupNavToggle() {
    const navBtn = document.getElementById('nav-toggle');
    const sidebar = document.getElementById('sidebar');
    if (!navBtn || !sidebar) return;

    navBtn.addEventListener('click', function () {
      sidebar.classList.toggle('open');
    });

    // Close sidebar when clicking outside of it
    document.addEventListener('click', function (e) {
      if (!sidebar.contains(e.target) && e.target !== navBtn) {
        sidebar.classList.remove('open');
      }
    });
  }

  // --- Landing-page code tabs ---
  function setupHeroTabs() {
    const tabs = document.querySelectorAll('.hero-tab');
    if (!tabs.length) return;
    tabs.forEach(function (tab) {
      tab.addEventListener('click', function () {
        const id = tab.getAttribute('data-tab');
        document.querySelectorAll('.hero-tab').forEach(function (t) {
          t.classList.toggle('active', t === tab);
        });
        document.querySelectorAll('.hero-panel').forEach(function (p) {
          p.classList.toggle('active', p.getAttribute('data-panel') === id);
        });
      });
    });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function () {
      setupThemeToggle();
      setupNavToggle();
      setupHeroTabs();
    });
  } else {
    setupThemeToggle();
    setupNavToggle();
    setupHeroTabs();
  }
})();
