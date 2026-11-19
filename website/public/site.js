// Theme controls, copy buttons, diagram tooltips and old landing anchors.
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
    event.preventDefault();
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

  var diagram = document.querySelector('[data-arch]');
  if (diagram) {
    var active;
    var pinned = false;
    var hideTimer;

    function positionTip() {
      var bounds = diagram.getBoundingClientRect();
      var anchor = active.trigger.getBoundingClientRect();
      var tip = active.tip;
      var left = anchor.left + (anchor.width - tip.offsetWidth) / 2 - bounds.left;
      var above = anchor.top - tip.offsetHeight - 6;
      var top = above >= 64 ? above : anchor.bottom + 6;
      tip.style.left = Math.max(0, Math.min(left, bounds.width - tip.offsetWidth)) + 'px';
      tip.style.top = top - bounds.top + 'px';
    }

    function hideTip() {
      clearTimeout(hideTimer);
      if (!active) return;
      active.tip.hidden = true;
      active.trigger.removeAttribute('data-arch-active');
      active = undefined;
      pinned = false;
    }

    function showTip(trigger, tip) {
      clearTimeout(hideTimer);
      if (active && active.trigger === trigger) return;
      hideTip();
      active = { trigger: trigger, tip: tip };
      trigger.setAttribute('data-arch-active', '');
      tip.hidden = false;
      positionTip();
    }

    diagram.querySelectorAll('[data-arch-trigger]').forEach(function (trigger) {
      var tip = document.getElementById(trigger.getAttribute('aria-describedby'));
      function leave() {
        if (active && active.trigger === trigger && !pinned && document.activeElement !== trigger) {
          hideTimer = setTimeout(hideTip, 160);
        }
      }
      function toggle() {
        if (active && active.trigger === trigger && pinned) return hideTip();
        showTip(trigger, tip);
        pinned = true;
      }
      trigger.addEventListener('pointerenter', function (event) {
        if (event.pointerType === 'mouse') showTip(trigger, tip);
      });
      trigger.addEventListener('pointerleave', leave);
      tip.addEventListener('pointerenter', function () { clearTimeout(hideTimer); });
      tip.addEventListener('pointerleave', leave);
      trigger.addEventListener('focus', function () { showTip(trigger, tip); });
      trigger.addEventListener('blur', function () {
        if (active && active.trigger === trigger) hideTip();
      });
      trigger.addEventListener('click', toggle);
      trigger.addEventListener('keydown', function (event) {
        if (event.key !== 'Enter' && event.key !== ' ') return;
        event.preventDefault();
        if (!event.repeat) toggle();
      });
    });

    document.addEventListener('keydown', function (event) {
      if (event.key === 'Escape') hideTip();
    });
    document.addEventListener('click', function (event) {
      if (active && !active.trigger.contains(event.target) && !active.tip.contains(event.target)) hideTip();
    });
    window.addEventListener('resize', function () { if (active) positionTip(); });
  }

  // The landing page used to be the whole README; send its old anchors to the docs.
  var id = decodeURIComponent(location.hash.slice(1));
  if (location.pathname === '/' && id && !document.getElementById(id)) location.replace('/docs/#' + encodeURIComponent(id));
})();
