export type Theme = "light" | "dark" | "system";

const THEME_KEY = "clipboardx_theme";

export function getTheme(): Theme {
  return (localStorage.getItem(THEME_KEY) as Theme) ?? "system";
}

function applyDark(isDark: boolean) {
  if (isDark) {
    document.documentElement.classList.add("dark");
    document.body.setAttribute("arco-theme", "dark");
  } else {
    document.documentElement.classList.remove("dark");
    document.body.removeAttribute("arco-theme");
  }
}

let mediaListener: (() => void) | null = null;

export function setTheme(theme: Theme) {
  localStorage.setItem(THEME_KEY, theme);

  if (mediaListener) {
    window.matchMedia("(prefers-color-scheme: dark)").removeEventListener("change", mediaListener);
    mediaListener = null;
  }

  if (theme === "system") {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    mediaListener = () => applyDark(mq.matches);
    mq.addEventListener("change", mediaListener);
    applyDark(mq.matches);
  } else {
    applyDark(theme === "dark");
  }
}

export function initTheme() {
  setTheme(getTheme());
}
