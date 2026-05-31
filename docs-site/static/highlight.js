// Lin syntax highlighter
// Applies token-class spans to <code class="language-lin"> blocks

(function () {
  'use strict';

  const KEYWORDS = [
    'val', 'var', 'if', 'then', 'else', 'match', 'is', 'has', 'when',
    'import', 'export', 'from', 'type', 'true', 'false', 'null', 'as'
  ];

  // Escape HTML entities
  function escapeHtml(text) {
    return text
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  // Tokenise a single line of Lin code into an HTML string with spans
  function highlightLine(line) {
    // We'll build a list of tokens: { type, value }
    // then join them into HTML.
    const tokens = [];
    let i = 0;
    const len = line.length;

    while (i < len) {
      // Comment
      if (line[i] === '/' && line[i + 1] === '/') {
        tokens.push({ type: 'cmt', value: line.slice(i) });
        break;
      }

      // String literal (double-quoted)
      if (line[i] === '"') {
        let j = i + 1;
        while (j < len) {
          if (line[j] === '\\') { j += 2; continue; }
          if (line[j] === '"') { j++; break; }
          j++;
        }
        tokens.push({ type: 'str', value: line.slice(i, j) });
        i = j;
        continue;
      }

      // Number literal
      if (/[0-9]/.test(line[i]) || (line[i] === '-' && /[0-9]/.test(line[i + 1] || ''))) {
        let j = i;
        if (line[j] === '-') j++;
        while (j < len && /[0-9_xXbBoO]/.test(line[j])) j++;
        if (j < len && line[j] === '.') {
          j++;
          while (j < len && /[0-9_]/.test(line[j])) j++;
        }
        if (j < len && (line[j] === 'e' || line[j] === 'E')) {
          j++;
          if (j < len && (line[j] === '+' || line[j] === '-')) j++;
          while (j < len && /[0-9]/.test(line[j])) j++;
        }
        // Optional type suffix
        while (j < len && /[a-zA-Z0-9]/.test(line[j])) j++;
        tokens.push({ type: 'num', value: line.slice(i, j) });
        i = j;
        continue;
      }

      // Identifier, keyword, or type
      if (/[a-zA-Z_]/.test(line[i])) {
        let j = i;
        while (j < len && /[a-zA-Z0-9_]/.test(line[j])) j++;
        const word = line.slice(i, j);
        if (KEYWORDS.includes(word)) {
          tokens.push({ type: 'kw', value: word });
        } else if (/^[A-Z]/.test(word)) {
          tokens.push({ type: 'typ', value: word });
        } else {
          tokens.push({ type: 'plain', value: word });
        }
        i = j;
        continue;
      }

      // Plain character
      tokens.push({ type: 'plain', value: line[i] });
      i++;
    }

    return tokens.map(t => {
      const v = escapeHtml(t.value);
      if (t.type === 'plain') return v;
      return `<span class="${t.type}">${v}</span>`;
    }).join('');
  }

  function highlightBlock(block) {
    const lines = block.textContent.split('\n');
    const highlighted = lines.map(highlightLine).join('\n');
    block.innerHTML = highlighted;
  }

  function run() {
    const blocks = document.querySelectorAll('code.language-lin');
    blocks.forEach(highlightBlock);
    // Also highlight generic <pre><code> without a language class
    const plain = document.querySelectorAll('pre code:not([class])');
    plain.forEach(highlightBlock);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', run);
  } else {
    run();
  }
})();
