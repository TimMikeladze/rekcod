// Theme controls, copy buttons and old landing anchors. Everything on the page works
// without this file except copying and switching the theme by hand.
(function () {
  var order = ['system', 'dark', 'light'];
  var theme = window.rekcodTheme;
  var status = document.getElementById('copy-status');

  function label() {
    var p = document.documentElement.getAttribute('data-theme-pref') || 'system';
    document.querySelectorAll('[data-theme-toggle]').forEach(function (b) {
      b.setAttribute('aria-label', 'Theme: ' + p + '. Click to change');
    });
    document.querySelectorAll('[data-set-theme]').forEach(function (b) {
      b.setAttribute('aria-pressed', String(b.getAttribute('data-set-theme') === p));
    });
  }
  function set(p) {
    try {
      if (p === 'system') localStorage.removeItem(theme.key);
      else localStorage.setItem(theme.key, p);
    } catch (e) {}
    theme.apply();
    label();
    document.dispatchEvent(new CustomEvent('rekcod-theme'));
  }
  if (theme) {
    document.querySelectorAll('[data-theme-toggle]').forEach(function (b) {
      b.addEventListener('click', function () {
        var p = document.documentElement.getAttribute('data-theme-pref') || 'system';
        set(order[(order.indexOf(p) + 1) % order.length]);
      });
    });
    document.querySelectorAll('[data-set-theme]').forEach(function (b) {
      b.addEventListener('click', function () { set(b.getAttribute('data-set-theme')); });
    });
    new MutationObserver(label).observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme-pref'] });
    label();
  }

  function announce(text) { if (status) status.textContent = text; }
  function copy(button, text) {
    navigator.clipboard.writeText(text).then(function () {
      button.setAttribute('data-copied', '');
      announce('Copied to clipboard.');
      setTimeout(function () { button.removeAttribute('data-copied'); }, 1600);
    }, function () { announce('Copy failed. Select the text and copy it with your keyboard.'); });
  }
  document.addEventListener('click', function (event) {
    var button = event.target.closest && event.target.closest('[data-copy],[data-copy-url]');
    if (!button) return;
    if (button.hasAttribute('data-copy')) return copy(button, button.getAttribute('data-copy'));
    fetch(button.getAttribute('data-copy-url')).then(function (r) { return r.text(); }).then(function (t) { copy(button, t); });
  });

  // Code blocks in the reference page get a copy button.
  document.querySelectorAll('.prose pre:not(.mermaid)').forEach(function (pre) {
    var code = pre.querySelector('code');
    if (!code) return;
    var b = document.createElement('button');
    b.type = 'button';
    b.className = 'copy-code';
    b.textContent = 'Copy';
    b.setAttribute('aria-label', 'Copy code');
    b.addEventListener('click', function () { copy(b, code.textContent || ''); });
    pre.append(b);
  });

  // The landing page used to be the whole README; send its old anchors to the docs.
  var id = decodeURIComponent(location.hash.slice(1));
  if (location.pathname === '/' && id && !document.getElementById(id)) location.replace('/docs/#' + encodeURIComponent(id));
})();
