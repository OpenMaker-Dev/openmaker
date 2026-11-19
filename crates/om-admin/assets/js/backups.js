// Backups (src/views/backups.rs). The tab works through its forms without
// this script; it adds:
// - the Backup Schedule and Showing selects take effect when changed;
// - Filter by Label hides the rows whose label does not match while the
//   administrator types, and counts the rows shown;
// - Edit Label shows the checked backup's current label, and its OK waits
//   for a new one.
"use strict";

(function () {
  document.addEventListener("change", function (event) {
    var target = event.target;
    if (target.matches && target.matches("select[data-submit-on-change]") && target.form) {
      if (target.form.requestSubmit) {
        target.form.requestSubmit();
      } else {
        target.form.submit();
      }
    }
  });

  // A hidden row's checkbox is unchecked and disabled, so that the forms
  // never act on a backup the administrator cannot see, and a row shown
  // again comes back unchecked (even if the browser restored its state).
  function filterRows(input) {
    var table = document.getElementById(input.getAttribute("data-filter-rows"));
    if (!table) {
      return;
    }
    var text = input.value.trim().toLowerCase();
    var shown = 0;
    var rows = table.querySelectorAll("tbody tr[data-label]");
    rows.forEach(function (row) {
      var visible = text === "" || row.getAttribute("data-label").toLowerCase().indexOf(text) !== -1;
      row.hidden = !visible;
      var box = row.querySelector("input[type=checkbox]");
      if (box) {
        if (!visible || box.disabled) {
          box.checked = false;
        }
        box.disabled = !visible;
      }
      if (visible) {
        shown += 1;
      }
    });
    var none = table.querySelector("tr.filter-empty");
    if (none) {
      none.hidden = rows.length === 0 || shown > 0;
    }
    var all = table.querySelector("input[data-select-all]");
    if (all) {
      all.disabled = shown === 0;
    }
    document.querySelectorAll("[data-shown-count]").forEach(function (count) {
      count.textContent = String(shown);
    });
    // The shared script recounts the selection on a change.
    table.dispatchEvent(new Event("change", { bubbles: true }));
  }

  document.querySelectorAll("input[data-filter-rows]").forEach(function (input) {
    input.addEventListener("input", function () {
      filterRows(input);
    });
  });

  var modal = document.getElementById("edit-label");
  if (!modal) {
    return;
  }
  var field = modal.querySelector("input[name=new_label]");
  var ok = modal.querySelector("button[name=relabel]");
  var current = modal.querySelector("[data-current-label]");
  var note = modal.querySelector("[data-label-note]");

  function updateOk() {
    ok.disabled = field.value.trim() === "";
  }

  function prepare() {
    if (location.hash !== "#edit-label") {
      return;
    }
    var checked = Array.prototype.filter.call(
      document.querySelectorAll("input[type=checkbox][name=backup]"),
      function (c) {
        return c.checked && !c.disabled;
      }
    );
    if (checked.length === 1) {
      var row = checked[0].closest("tr");
      current.querySelector(".current-label").textContent = row ? row.getAttribute("data-label") : "";
      current.hidden = false;
      note.hidden = true;
    } else {
      current.hidden = true;
      note.hidden = false;
      note.textContent =
        checked.length > 1
          ? "You checked " + checked.length + " backups. Each gets the new label."
          : "Each checked backup gets the new label.";
    }
    updateOk();
  }

  field.addEventListener("input", updateOk);
  window.addEventListener("hashchange", prepare);
  updateOk();
  prepare();
})();
