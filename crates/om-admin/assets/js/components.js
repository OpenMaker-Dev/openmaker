// Shared components (src/components.rs). The console works without this
// script; it adds:
// - live regions ([data-refresh]) re-fetched every 5 s while the page is
//   visible, keeping the checkboxes checked in them, and left alone while
//   the administrator is using them;
// - buttons that need a checked row ([data-needs-selection]) disabled until
//   one is checked, and "select all" checkboxes ([data-select-all]);
// - help popovers and menus closed by an outside click or Escape, and
//   modals closed by Escape, with the focus on their first field.
"use strict";

(function () {
  var REFRESH_MS = 5000;

  // The checkboxes named `name` that a form would send: a disabled one,
  // such as a filtered-out row's, is neither selected by "select all" nor
  // counted.
  function boxes(name) {
    return Array.prototype.filter.call(
      document.querySelectorAll("input[type=checkbox]"),
      function (c) {
        return c.name === name && !c.disabled;
      }
    );
  }

  function updateSelection() {
    document.querySelectorAll("[data-needs-selection]").forEach(function (b) {
      var any = boxes(b.getAttribute("data-needs-selection")).some(function (c) {
        return c.checked;
      });
      if (b.tagName === "A") {
        b.setAttribute("aria-disabled", String(!any));
      } else {
        b.disabled = !any;
      }
    });
    document.querySelectorAll("input[data-select-all]").forEach(function (all) {
      var list = boxes(all.getAttribute("data-select-all"));
      all.checked = list.length > 0 && list.every(function (c) {
        return c.checked;
      });
    });
  }

  document.addEventListener("change", function (event) {
    var target = event.target;
    if (target.matches && target.matches("input[data-select-all]")) {
      boxes(target.getAttribute("data-select-all")).forEach(function (c) {
        c.checked = target.checked;
      });
    }
    updateSelection();
  });

  document.addEventListener("click", function (event) {
    var disabled = event.target.closest && event.target.closest("a[aria-disabled=true]");
    if (disabled) {
      event.preventDefault();
    }
    var inside = event.target.closest ? event.target.closest("details.help, details.menu") : null;
    document.querySelectorAll("details.help[open], details.menu[open]").forEach(function (d) {
      if (d !== inside) {
        d.open = false;
      }
    });
  });

  function openModal() {
    if (!location.hash || location.hash.length < 2) {
      return null;
    }
    var target = document.getElementById(decodeURIComponent(location.hash.slice(1)));
    return target && target.classList.contains("modal") ? target : null;
  }

  function focusModal() {
    var modal = openModal();
    if (modal) {
      var field = modal.querySelector("input:not([type=hidden]), textarea, select, button");
      if (field) {
        field.focus();
      }
    }
  }

  window.addEventListener("hashchange", focusModal);
  focusModal();

  document.addEventListener("keydown", function (event) {
    if (event.key !== "Escape") {
      return;
    }
    document.querySelectorAll("details.help[open], details.menu[open]").forEach(function (d) {
      d.open = false;
    });
    if (openModal()) {
      location.hash = "";
    }
  });

  // A region is left alone while a modal is open, a menu in it is open, or
  // the administrator is typing in it.
  function busy(region) {
    if (openModal() || region.querySelector("details[open]")) {
      return true;
    }
    var active = document.activeElement;
    return Boolean(
      active && region.contains(active) && active.matches("input:not([type=checkbox]), textarea, select")
    );
  }

  var stopped = false;
  // The regions with a fetch outstanding: a region waits for its answer
  // before it asks again, so an older answer never replaces a newer one.
  var pending = new Set();

  function refresh(region) {
    if (pending.has(region)) {
      return;
    }
    pending.add(region);
    fetch(region.getAttribute("data-refresh"), {
      credentials: "same-origin",
      headers: { Accept: "text/html" },
    })
      .then(function (response) {
        if (response.status === 401) {
          // The session has ended: the page's reload shows the sign-in.
          stopped = true;
          location.reload();
          return null;
        }
        return response.ok ? response.text() : null;
      })
      .then(function (markup) {
        if (markup === null || stopped || busy(region)) {
          return;
        }
        var checked = {};
        region.querySelectorAll("input[type=checkbox]:checked").forEach(function (c) {
          checked[c.name + "\u0000" + c.value] = true;
        });
        // Rendered by the console itself, with all text escaped.
        region.innerHTML = markup;
        region.querySelectorAll("input[type=checkbox]").forEach(function (c) {
          if (checked[c.name + "\u0000" + c.value]) {
            c.checked = true;
          }
        });
        updateSelection();
      })
      .catch(function () {})
      .then(function () {
        pending.delete(region);
      });
  }

  if (document.querySelector("[data-refresh]")) {
    window.setInterval(function () {
      if (stopped || document.visibilityState !== "visible") {
        return;
      }
      document.querySelectorAll("[data-refresh]").forEach(function (region) {
        if (!busy(region)) {
          refresh(region);
        }
      });
    }, REFRESH_MS);
  }

  updateSelection();
})();
