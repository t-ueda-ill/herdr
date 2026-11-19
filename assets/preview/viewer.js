// Renders the file named by #herdr-view[data-src] in the format herdr chose.
// Runs under a Content-Security-Policy that only allows herdr's own scripts,
// so markup inside a rendered file can show but never run.
(function () {
  'use strict';

  var view = document.getElementById('herdr-view');
  if (!view) return;

  var MAX_HIGHLIGHT = 1024 * 1024;
  var MAX_ROWS = 5000;
  var dark = !!(window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches);

  function escapeHtml(text) {
    return text.replace(/[&<>"']/g, function (ch) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[ch];
    });
  }

  function highlighted(code, language) {
    if (!window.hljs || code.length > MAX_HIGHLIGHT) return escapeHtml(code);
    if (language && window.hljs.getLanguage(language)) {
      return window.hljs.highlight(code, { language: language, ignoreIllegals: true }).value;
    }
    return escapeHtml(code);
  }

  function rendered() {
    document.dispatchEvent(new Event('herdr:rendered'));
  }

  function showError(message) {
    view.className = 'herdr-message';
    view.textContent = '';
    var heading = document.createElement('h1');
    heading.textContent = 'Could not show this file';
    var detail = document.createElement('p');
    detail.textContent = message;
    view.appendChild(heading);
    view.appendChild(detail);
    rendered();
  }

  function loadMermaid(then) {
    if (window.mermaid) return then();
    var script = document.createElement('script');
    script.src = '/__herdr/assets/mermaid.min.js';
    script.onload = then;
    script.onerror = function () { showError('The diagram renderer did not load.'); };
    document.head.appendChild(script);
  }

  // securityLevel 'strict' keeps click callbacks and HTML labels in a
  // diagram from running anything.
  function renderMermaid(nodes, done) {
    if (!nodes.length) return done();
    loadMermaid(function () {
      window.mermaid.initialize({
        startOnLoad: false,
        securityLevel: 'strict',
        theme: dark ? 'dark' : 'default'
      });
      // Why not suppressErrors: a diagram with a syntax error should say so
      // in the console instead of staying as plain text without a reason.
      window.mermaid.run({ nodes: nodes }).then(done, function (error) {
        console.error('mermaid:', error && error.message || error);
        done();
      });
    });
  }

  function stripFrontMatter(text) {
    var match = /^---\r?\n[\s\S]*?\r?\n(?:---|\.\.\.)[ \t]*(?:\r?\n|$)/.exec(text);
    return match ? text.slice(match[0].length) : text;
  }

  function slug(text, used) {
    var base = text.trim().toLowerCase()
      .replace(/[^\p{L}\p{N}\s_-]/gu, '')
      .replace(/\s+/g, '-') || 'section';
    var candidate = base;
    for (var n = 1; used[candidate]; n++) candidate = base + '-' + n;
    used[candidate] = true;
    return candidate;
  }

  function addHeadingAnchors(root) {
    var used = {};
    root.querySelectorAll('h1, h2, h3, h4, h5, h6').forEach(function (heading) {
      if (!heading.id) heading.id = slug(heading.textContent, used);
      else used[heading.id] = true;
    });
  }

  function addTaskBoxes(root) {
    root.querySelectorAll('li').forEach(function (item) {
      var first = item.firstChild;
      if (first && first.nodeName === 'P') first = first.firstChild;
      if (!first || first.nodeType !== Node.TEXT_NODE) return;
      var match = /^\[([ xX])\]\s/.exec(first.nodeValue);
      if (!match) return;
      first.nodeValue = first.nodeValue.slice(match[0].length);
      var box = document.createElement('input');
      box.type = 'checkbox';
      box.disabled = true;
      box.checked = match[1] !== ' ';
      first.parentNode.insertBefore(box, first);
      item.classList.add('task-list-item');
    });
  }

  function renderMarkdown(text) {
    var md = window.markdownit({
      html: true,
      linkify: true,
      highlight: function (code, language) {
        return language === 'mermaid' ? '' : highlighted(code, language);
      }
    });
    if (window.markdownitFootnote) md.use(window.markdownitFootnote);
    // Why innerHTML: elements inserted this way never run their <script>,
    // and the page policy blocks inline handlers such as onerror.
    view.className = 'markdown-body';
    view.innerHTML = md.render(stripFrontMatter(text));
    view.querySelectorAll('pre > code').forEach(function (code) {
      code.classList.add('hljs');
    });
    addTaskBoxes(view);
    addHeadingAnchors(view);
    var diagrams = [];
    view.querySelectorAll('pre > code.language-mermaid').forEach(function (code) {
      var diagram = document.createElement('div');
      diagram.className = 'mermaid';
      diagram.textContent = code.textContent;
      code.parentNode.replaceWith(diagram);
      diagrams.push(diagram);
    });
    renderMermaid(diagrams, function () {
      if (location.hash) {
        var target = document.getElementById(decodeURIComponent(location.hash.slice(1)));
        if (target) target.scrollIntoView();
      }
      rendered();
    });
  }

  function renderDiagram(text) {
    view.className = 'herdr-diagram';
    var diagram = document.createElement('div');
    diagram.className = 'mermaid';
    diagram.textContent = text;
    view.appendChild(diagram);
    renderMermaid([diagram], rendered);
  }

  function parseDelimited(text, separator) {
    var rows = [];
    var row = [];
    var field = '';
    var quoted = false;
    for (var i = 0; i < text.length; i++) {
      var ch = text[i];
      if (quoted) {
        if (ch === '"' && text[i + 1] === '"') { field += '"'; i++; }
        else if (ch === '"') quoted = false;
        else field += ch;
      } else if (ch === '"' && field === '') {
        quoted = true;
      } else if (ch === separator) {
        row.push(field); field = '';
      } else if (ch === '\n' || ch === '\r') {
        if (ch === '\r' && text[i + 1] === '\n') i++;
        row.push(field); field = '';
        rows.push(row); row = [];
        if (rows.length > MAX_ROWS) break;
      } else {
        field += ch;
      }
    }
    if (field !== '' || row.length) { row.push(field); rows.push(row); }
    return rows;
  }

  function renderTable(text, separator) {
    var rows = parseDelimited(text, separator);
    var truncated = rows.length > MAX_ROWS;
    if (truncated) rows = rows.slice(0, MAX_ROWS);
    view.className = 'herdr-table';
    var table = document.createElement('table');
    rows.forEach(function (cells, index) {
      var section = index === 0 ? (table.tHead || table.createTHead()) : (table.tBodies[0] || table.createTBody());
      var tr = section.insertRow();
      cells.forEach(function (cell) {
        var td = document.createElement(index === 0 ? 'th' : 'td');
        td.textContent = cell;
        tr.appendChild(td);
      });
    });
    view.appendChild(table);
    if (truncated) {
      var note = document.createElement('p');
      note.className = 'herdr-note';
      note.textContent = 'First ' + MAX_ROWS + ' rows';
      view.appendChild(note);
    }
    rendered();
  }

  function renderCode(text, language) {
    view.className = 'herdr-code';
    var lines = text.split('\n');
    if (lines.length > 1 && lines[lines.length - 1] === '') lines.pop();
    var gutter = document.createElement('pre');
    gutter.className = 'herdr-gutter';
    gutter.setAttribute('aria-hidden', 'true');
    gutter.textContent = lines.map(function (_, i) { return i + 1; }).join('\n');
    var pre = document.createElement('pre');
    var code = document.createElement('code');
    code.className = 'hljs';
    code.innerHTML = highlighted(lines.join('\n'), language);
    pre.appendChild(code);
    view.appendChild(gutter);
    view.appendChild(pre);
    rendered();
  }

  function renderJson(text) {
    var pretty = text;
    try { pretty = JSON.stringify(JSON.parse(text), null, 2); } catch (e) { /* shown as written */ }
    renderCode(pretty, 'json');
  }

  function render(text) {
    switch (view.dataset.format) {
      case 'markdown': return renderMarkdown(text);
      case 'mermaid': return renderDiagram(text);
      case 'csv': return renderTable(text, ',');
      case 'tsv': return renderTable(text, '\t');
      case 'json': return renderJson(text);
      default: return renderCode(text, view.dataset.language);
    }
  }

  fetch(view.dataset.src, { cache: 'no-store' })
    .then(function (response) {
      if (!response.ok) throw new Error(response.status + ' ' + response.statusText);
      return response.text();
    })
    .then(render)
    .catch(function (error) { showError(String(error && error.message || error)); });
})();
