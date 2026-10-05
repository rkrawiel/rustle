/* Keyboard shortcuts and the help overlay. Progressive enhancement only: every shortcut
 * here submits or follows something already present in the markup (a link, or a form
 * also reachable by clicking its own button), so nothing here duplicates server logic
 * and every page works identically with this file blocked. The one exception the brief
 * itself calls out is `v` (open original in a new tab), which has no keyboard-free
 * equivalent beyond clicking the same link. */

(function () {
  "use strict";

  /* Shortcuts must not fire while the reader is typing, including into a select or a
   * contenteditable area. */
  function isTyping(target) {
    const tag = target.tagName;
    return (
      target.isContentEditable || tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT"
    );
  }

  function items() {
    return Array.from(document.querySelectorAll(".items > .item"));
  }

  let selected = -1;

  function select(index, list) {
    if (list.length === 0) {
      return;
    }
    if (selected >= 0 && list[selected]) {
      list[selected].classList.remove("is-selected");
    }
    selected = Math.max(0, Math.min(index, list.length - 1));
    const item = list[selected];
    item.classList.add("is-selected");
    item.scrollIntoView({ block: "nearest" });
  }

  function formEndingWith(scope, suffix) {
    return Array.from(scope.querySelectorAll("form")).find((form) =>
      (form.getAttribute("action") || "").endsWith(suffix)
    );
  }

  function openOriginal(scope) {
    const link = scope && scope.querySelector("[data-original-link]");
    if (link) {
      window.open(link.getAttribute("href"), "_blank", "noopener,noreferrer");
    }
  }

  const help = document.getElementById("shortcut-help");
  let lastFocused = null;

  function openHelp() {
    if (!help) {
      return;
    }
    lastFocused = document.activeElement;
    help.hidden = false;
    const close = document.getElementById("shortcut-help-close");
    if (close) {
      close.focus();
    }
  }

  function closeHelp() {
    if (!help || help.hidden) {
      return;
    }
    help.hidden = true;
    if (lastFocused && typeof lastFocused.focus === "function") {
      lastFocused.focus();
    }
  }

  const openButton = document.getElementById("shortcut-help-open");
  if (openButton) {
    openButton.addEventListener("click", openHelp);
  }
  const closeButton = document.getElementById("shortcut-help-close");
  if (closeButton) {
    closeButton.addEventListener("click", closeHelp);
  }
  if (help) {
    // Clicking the dimmed backdrop closes it, same as a modal dialog elsewhere.
    help.addEventListener("click", (event) => {
      if (event.target === help) {
        closeHelp();
      }
    });
  }

  const GOTO = { u: "/unread", s: "/starred", f: "/feeds", c: "/categories" };
  let chord = null;

  document.addEventListener("keydown", (event) => {
    if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) {
      return;
    }
    if (isTyping(event.target)) {
      return;
    }

    if (help && !help.hidden) {
      if (event.key === "Escape" || event.key === "?") {
        event.preventDefault();
        closeHelp();
      }
      return;
    }

    if (chord === "g") {
      chord = null;
      const target = GOTO[event.key];
      if (target) {
        event.preventDefault();
        window.location.assign(target);
      }
      return;
    }

    switch (event.key) {
      case "g":
        chord = "g";
        return;

      case "j":
      case "k": {
        const list = items();
        if (list.length > 0) {
          event.preventDefault();
          select(selected + (event.key === "j" ? 1 : -1), list);
          break;
        }
        // No list on this page: we are reading one entry, so j/k walk to the next or
        // previous entry the same `published_at` order would, via the links already
        // rendered for exactly that.
        const nav = document.querySelector(
          `[data-nav="${event.key === "j" ? "older" : "newer"}"]`
        );
        if (nav) {
          event.preventDefault();
          window.location.assign(nav.getAttribute("href"));
        }
        break;
      }

      case "o":
      case "Enter": {
        const list = items();
        const item = list[selected];
        const link = item && item.querySelector(".item-title");
        if (link) {
          event.preventDefault();
          window.location.assign(link.getAttribute("href"));
        }
        break;
      }

      case "v": {
        const list = items();
        event.preventDefault();
        openOriginal(list[selected] || document);
        break;
      }

      case "m": {
        const list = items();
        const item = list[selected];
        const form = item
          ? formEndingWith(item, "/read")
          : document.getElementById("entry-read-form");
        if (form) {
          event.preventDefault();
          form.requestSubmit();
        }
        break;
      }

      case "s": {
        const list = items();
        const item = list[selected];
        const form = item
          ? formEndingWith(item, "/star")
          : document.getElementById("entry-star-form");
        if (form) {
          event.preventDefault();
          form.requestSubmit();
        }
        break;
      }

      case "A": {
        const form = document.getElementById("mark-all-read-form");
        if (form) {
          event.preventDefault();
          form.requestSubmit();
        }
        break;
      }

      case "r": {
        const form = document.getElementById("refresh-feed-form");
        if (form) {
          event.preventDefault();
          form.requestSubmit();
        }
        break;
      }

      case "/": {
        const search = document.getElementById("search-q");
        if (search) {
          event.preventDefault();
          search.focus();
        }
        break;
      }

      case "?":
        event.preventDefault();
        openHelp();
        break;

      default:
        break;
    }
  });
})();
