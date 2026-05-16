"use client";

import * as React from "react";
import { Moon, Sun } from "lucide-react";
import { Button } from "@/components/ui/button";

// `useSyncExternalStore` lets us read localStorage/system theme without a
// setState-in-useEffect dance, which is the source of the previous hydration
// mismatch. The server snapshot always returns false so SSR + first client
// render emit the same DOM; after hydration the real client snapshot kicks in
// and the icon swaps to match the user's preference.
function subscribeTheme(callback: () => void) {
  const onStorage = () => callback();
  const mql = window.matchMedia("(prefers-color-scheme: dark)");
  window.addEventListener("storage", onStorage);
  mql.addEventListener("change", onStorage);
  return () => {
    window.removeEventListener("storage", onStorage);
    mql.removeEventListener("change", onStorage);
  };
}

function readClientDark(): boolean {
  const stored = window.localStorage.getItem("theme");
  if (stored === "dark") return true;
  if (stored === "light") return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

export function ThemeToggle() {
  const isDark = React.useSyncExternalStore(
    subscribeTheme,
    readClientDark,
    () => false,
  );
  const mounted = React.useSyncExternalStore(
    () => () => {},
    () => true,
    () => false,
  );

  React.useEffect(() => {
    if (!mounted) return;
    document.documentElement.classList.toggle("dark", isDark);
  }, [isDark, mounted]);

  function toggle() {
    const next = !isDark;
    window.localStorage.setItem("theme", next ? "dark" : "light");
    // `storage` events fire across tabs; dispatch one in this tab so
    // useSyncExternalStore's subscribers re-read the snapshot immediately.
    window.dispatchEvent(new StorageEvent("storage", { key: "theme" }));
  }

  return (
    <Button
      variant="ghost"
      size="icon"
      aria-label="Toggle theme"
      onClick={toggle}
    >
      {/* Server + first-client render produce the same placeholder. */}
      {!mounted ? (
        <span className="h-5 w-5" aria-hidden="true" />
      ) : isDark ? (
        <Sun className="h-5 w-5" />
      ) : (
        <Moon className="h-5 w-5" />
      )}
    </Button>
  );
}
