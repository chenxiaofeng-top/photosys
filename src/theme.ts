import { readonly, ref } from "vue";

export type ThemePreference = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

const storageKey = "photosys.theme";
const preference = ref<ThemePreference>("system");
const resolved = ref<ResolvedTheme>("light");
let systemTheme: MediaQueryList | undefined;
let initialized = false;

export const themePreference = readonly(preference);
export const resolvedTheme = readonly(resolved);

function applyTheme() {
  resolved.value = preference.value === "system"
    ? systemTheme?.matches ? "dark" : "light"
    : preference.value;
  document.documentElement.dataset.theme = resolved.value;
}

export function setThemePreference(value: ThemePreference) {
  preference.value = value;
  applyTheme();
  try { window.localStorage.setItem(storageKey, value); }
  catch { /* Theme changes also work in read-only or restricted webviews. */ }
}

/** Called before mounting Vue so the first rendered frame has the right theme. */
export function initializeTheme() {
  if (initialized) return;
  initialized = true;
  try {
    const saved = window.localStorage.getItem(storageKey);
    if (saved === "light" || saved === "dark" || saved === "system") preference.value = saved;
  } catch { /* Fall back to the system preference. */ }
  systemTheme = window.matchMedia("(prefers-color-scheme: dark)");
  systemTheme.addEventListener("change", applyTheme);
  applyTheme();
}

if (import.meta.hot) import.meta.hot.dispose(() => systemTheme?.removeEventListener("change", applyTheme));
