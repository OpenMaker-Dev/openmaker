// Databases (src/views/databases.rs). The view works without this script;
// it keeps a dialog's OK disabled until its fields are valid, as FileMaker
// Server's dialogs do: a required message that is not only spaces, and a
// delay that is empty or 0 to 60 minutes. The server checks them again.
"use strict";

(function () {
  function validDelay(text) {
    var value = text.trim();
    return value === "" || (/^[0-9]{1,2}$/.test(value) && Number(value) <= 60);
  }

  function update(modal) {
    var ok = modal.querySelector(".modal-actions button[type=submit]");
    if (!ok) {
      return;
    }
    var valid = true;
    modal.querySelectorAll("input[data-delay]").forEach(function (input) {
      valid = valid && validDelay(input.value);
    });
    modal.querySelectorAll("textarea[data-required]").forEach(function (text) {
      valid = valid && text.value.trim() !== "";
    });
    ok.disabled = !valid;
  }

  document.querySelectorAll(".activities .modal").forEach(function (modal) {
    modal.addEventListener("input", function () {
      update(modal);
    });
    update(modal);
  });

  // The Close dialog opens with the page (its address ends in
  // #close-database), which shows it only once the page has loaded: focus
  // its message then.
  window.addEventListener("load", function () {
    var modal = document.querySelector(".activities .modal:target");
    var field = modal && modal.querySelector("textarea, input:not([type=hidden])");
    if (field) {
      field.focus();
    }
  });
})();
