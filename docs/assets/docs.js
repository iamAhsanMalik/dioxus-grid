// Copy-to-clipboard for every code block.
//
// The button is added here rather than written into all 70 blocks by hand: the
// markup stays clean, and a page that adds a code block gets the button for free.
// Every block has a `.code-head` bar, which is where the button goes.
(function () {
  "use strict";

  var COPY =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
    '<rect x="9" y="9" width="12" height="12" rx="2"/>' +
    '<path d="M5 15V5a2 2 0 0 1 2-2h8"/></svg>';
  var DONE =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
    '<path d="m5 13 4 4L19 7"/></svg>';

  // `navigator.clipboard` needs a secure context; file:// and plain http lose it,
  // so fall back to a hidden textarea + execCommand rather than doing nothing.
  function write(text) {
    if (navigator.clipboard && window.isSecureContext) {
      return navigator.clipboard.writeText(text);
    }
    return new Promise(function (resolve, reject) {
      var ta = document.createElement("textarea");
      ta.value = text;
      // Off-screen but still focusable, and readonly so mobile keyboards stay shut.
      ta.setAttribute("readonly", "");
      ta.style.cssText = "position:fixed;top:-9999px;opacity:0";
      document.body.appendChild(ta);
      ta.select();
      ta.setSelectionRange(0, ta.value.length);
      var ok = false;
      try {
        ok = document.execCommand("copy");
      } catch (e) {
        ok = false;
      }
      document.body.removeChild(ta);
      ok ? resolve() : reject(new Error("copy failed"));
    });
  }

  document.querySelectorAll(".code").forEach(function (block) {
    var head = block.querySelector(".code-head");
    var code = block.querySelector("pre code");
    if (!head || !code) return;

    var btn = document.createElement("button");
    btn.type = "button";
    btn.className = "code-copy";
    btn.innerHTML = COPY;
    // The label names what gets copied, since the icon alone is ambiguous next to
    // a filename. `aria-live` announces the result to screen readers.
    var name = (head.firstElementChild || head).textContent.trim();
    btn.setAttribute("aria-label", name ? "Copy " + name : "Copy code");
    btn.title = "Copy";

    var status = document.createElement("span");
    status.className = "sr-only";
    status.setAttribute("aria-live", "polite");
    btn.appendChild(status);

    var timer;
    btn.addEventListener("click", function () {
      // `textContent` on the <code> gives the source without the token markup.
      write(code.textContent).then(
        function () {
          clearTimeout(timer);
          btn.innerHTML = DONE;
          btn.appendChild(status);
          btn.setAttribute("data-copied", "true");
          status.textContent = "Copied";
          timer = setTimeout(function () {
            btn.innerHTML = COPY;
            btn.appendChild(status);
            btn.removeAttribute("data-copied");
            status.textContent = "";
          }, 2000);
        },
        function () {
          status.textContent = "Copy failed";
        }
      );
    });

    head.appendChild(btn);
  });
})();
