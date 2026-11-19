// Configuration (src/views/folders.rs): a Copy button ([data-copy]) puts
// its text on the clipboard. The buttons stay hidden where the browser does
// not let a page write to the clipboard, so none does nothing.
"use strict";

(function () {
  if (!navigator.clipboard || !window.isSecureContext) {
    return;
  }
  document.querySelectorAll("button[data-copy]").forEach(function (button) {
    button.hidden = false;
  });
  document.addEventListener("click", function (event) {
    var button = event.target.closest ? event.target.closest("button[data-copy]") : null;
    if (!button) {
      return;
    }
    navigator.clipboard.writeText(button.getAttribute("data-copy")).then(
      function () {
        button.textContent = "Copied";
        button.classList.add("copied");
        window.setTimeout(function () {
          button.textContent = "Copy";
          button.classList.remove("copied");
        }, 1500);
      },
      function () {}
    );
  });
})();
