// Runs before React hydrates. Reads the user's saved theme (or system
// preference if unset) and applies the `dark` class to <html> synchronously,
// before any paint, so the page does not flash light-then-dark.
(function () {
  try {
    var saved = localStorage.getItem("theme");
    var prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    var dark = saved === "dark" || (saved !== "light" && prefersDark);
    if (dark) {
      document.documentElement.classList.add("dark");
    } else {
      document.documentElement.classList.remove("dark");
    }
  } catch {
    // No localStorage / matchMedia — leave the default (light) in place.
  }
})();
