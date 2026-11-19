// Reloads the page when herdr reports a change under the preview root, and
// puts the scroll position back afterwards.
(function () {
  'use strict';

  var key = 'herdr-scroll:' + location.pathname;

  try {
    var saved = sessionStorage.getItem(key);
    if (saved !== null) {
      sessionStorage.removeItem(key);
      var y = Number(saved);
      var restore = function () { window.scrollTo(0, y); };
      if (document.readyState === 'complete') restore();
      else window.addEventListener('load', restore);
      // Rendered formats draw after load, so restore again once they finish.
      document.addEventListener('herdr:rendered', restore);
    }
  } catch (e) { /* storage blocked: reload without restoring */ }

  if (!window.EventSource) return;
  var source = new EventSource('/__herdr/events');
  source.onmessage = function (event) {
    if (event.data !== 'reload') return;
    try { sessionStorage.setItem(key, String(window.scrollY)); } catch (e) { /* ignore */ }
    location.reload();
  };
})();
