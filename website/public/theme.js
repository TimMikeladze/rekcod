// Theme boot: runs before first paint, so it is a blocking script in <head>. The page
// is dark by default and follows the OS without it (global.css has the media query).
(function () {
  var key = 'rekcod-theme';
  var root = document.documentElement;
  var dark = matchMedia('(prefers-color-scheme: dark)');
  var light = matchMedia('(prefers-color-scheme: light)');
  function pref() {
    try {
      var stored = localStorage.getItem(key);
      return stored === 'dark' || stored === 'light' ? stored : 'system';
    } catch (e) {
      return 'system';
    }
  }
  function apply() {
    var p = pref();
    root.setAttribute('data-theme-pref', p);
    root.setAttribute('data-theme', p === 'system' ? (light.matches && !dark.matches ? 'light' : 'dark') : p);
  }
  apply();
  dark.addEventListener('change', apply);
  light.addEventListener('change', apply);
  window.rekcodTheme = { key: key, apply: apply };
})();
