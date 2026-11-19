// OpenMaker Server Admin Console. Written for OpenMaker. Every page works
// without this script: it keeps the top bar's clock running and lets it show
// the browser's time zone, closes the alerts panel on an outside click or
// Escape, dims Sign In until both fields are filled, and scrolls the tab row
// to the current tab on a narrow screen.
"use strict";

(function () {
  function clockText(date, timeZone) {
    var day = new Intl.DateTimeFormat("en-US", {
      weekday: "long",
      month: "long",
      day: "numeric",
      year: "numeric",
      timeZone: timeZone,
    }).format(date);
    var time = new Intl.DateTimeFormat("en-US", {
      hour: "2-digit",
      minute: "2-digit",
      hourCycle: "h23",
      timeZone: timeZone,
    }).format(date);
    return day + " at " + time;
  }

  // The server renders its own time and UTC offset; the browser's clock only
  // measures the time since.
  function setUpClock() {
    var clock = document.querySelector(".clock");
    if (!clock) {
      return;
    }
    var epoch = Number(clock.dataset.epoch);
    var offset = Number(clock.dataset.offset);
    if (!isFinite(epoch) || !isFinite(offset)) {
      return;
    }
    var skew = epoch - Date.now();
    var local = false;
    function render() {
      var now = Date.now() + skew;
      clock.textContent = local
        ? clockText(new Date(now), undefined)
        : clockText(new Date(now + offset * 1000), "UTC");
    }
    var toggle = document.querySelector(".time-toggle");
    if (toggle) {
      toggle.hidden = false;
      toggle.addEventListener("click", function () {
        local = !local;
        toggle.textContent = local ? "LOCAL" : "SERVER";
        toggle.setAttribute("aria-pressed", String(local));
        toggle.title = local
          ? "Show the server's time"
          : "Show the time in this browser's time zone";
        render();
      });
    }
    window.setInterval(render, 15000);
  }

  function setUpPopovers() {
    function closeAll(except) {
      document.querySelectorAll("details.bell[open]").forEach(function (d) {
        if (d !== except) {
          d.open = false;
        }
      });
    }
    document.addEventListener("click", function (event) {
      var inside = event.target.closest ? event.target.closest("details.bell") : null;
      closeAll(inside);
    });
    document.addEventListener("keydown", function (event) {
      if (event.key === "Escape") {
        closeAll(null);
      }
    });
  }

  // Dimmed, not disabled: a browser may fill a saved password without
  // telling the page until the user acts, and the fields' `required` already
  // stops an empty sign-in.
  function setUpSignIn() {
    var form = document.querySelector(".signin-form");
    if (!form) {
      return;
    }
    var fields = form.querySelectorAll("input[name=username], input[name=password]");
    var button = form.querySelector(".signin-button");
    function update() {
      var incomplete = Array.prototype.some.call(fields, function (f) {
        return f.value === "";
      });
      button.classList.toggle("is-incomplete", incomplete);
    }
    fields.forEach(function (f) {
      f.addEventListener("input", update);
      f.addEventListener("change", update);
    });
    update();
  }

  // On a narrow screen the tab row scrolls: bring the current tab into it.
  function showCurrentTab() {
    var row = document.querySelector(".tabs");
    var tab = row && row.querySelector(".tab.active");
    if (!tab || row.scrollWidth <= row.clientWidth) {
      return;
    }
    var left = tab.offsetLeft - row.offsetLeft;
    if (left < row.scrollLeft || left + tab.offsetWidth > row.scrollLeft + row.clientWidth) {
      row.scrollLeft = left - (row.clientWidth - tab.offsetWidth) / 2;
    }
  }

  setUpClock();
  setUpPopovers();
  setUpSignIn();
  showCurrentTab();
})();
