import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
// Load Vue without a fake document; these tests only exercise theme state.
import "vue";

const originalWindow = globalThis.window;
const originalDocument = globalThis.document;
let caseId = 0;

afterEach(() => {
  if (originalWindow === undefined) delete globalThis.window;
  else globalThis.window = originalWindow;
  if (originalDocument === undefined) delete globalThis.document;
  else globalThis.document = originalDocument;
});

async function browser({ saved = null, dark = false, storageFails = false } = {}) {
  const stored = new Map(saved === null ? [] : [["photosys.theme", saved]]);
  const listeners = new Set();
  const media = {
    matches: dark,
    addEventListener: (_, listener) => listeners.add(listener),
    removeEventListener: (_, listener) => listeners.delete(listener),
  };
  globalThis.document = { documentElement: { dataset: {} } };
  globalThis.window = {
    matchMedia(query) {
      assert.equal(query, "(prefers-color-scheme: dark)");
      return media;
    },
    get localStorage() {
      if (storageFails) throw new Error("Storage is unavailable");
      return { getItem: key => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value) };
    },
  };
  const theme = await import(`../src/theme.ts?test=${++caseId}`);
  theme.initializeTheme();
  return {
    theme, stored, listeners,
    system(value) { media.matches = value; for (const listener of listeners) listener(); },
    applied: () => document.documentElement.dataset.theme,
  };
}

test("first launch follows system and reacts to system changes", async () => {
  const b = await browser({ dark: true });
  assert.equal(b.theme.themePreference.value, "system");
  assert.equal(b.applied(), "dark");
  b.system(false);
  assert.equal(b.theme.resolvedTheme.value, "light");
  assert.equal(b.applied(), "light");
  b.theme.initializeTheme();
  assert.equal(b.listeners.size, 1);
});

test("saved choice overrides system; returning to system uses its current value", async () => {
  const b = await browser({ saved: "light", dark: true });
  assert.equal(b.applied(), "light");
  b.theme.setThemePreference("dark");
  assert.equal(b.stored.get("photosys.theme"), "dark");
  b.system(false);
  assert.equal(b.applied(), "dark");
  b.theme.setThemePreference("system");
  assert.equal(b.applied(), "light");
  assert.equal(b.stored.get("photosys.theme"), "system");
});

test("dark choice is restored before mount", async () => {
  const b = await browser({ saved: "dark", dark: false });
  assert.equal(b.applied(), "dark");
  assert.equal(b.theme.themePreference.value, "dark");
});

test("unknown saved values fall back to system", async () => {
  const b = await browser({ saved: "invalid", dark: true });
  assert.equal(b.theme.themePreference.value, "system");
  assert.equal(b.applied(), "dark");
});

test("blocked storage does not prevent startup or manual theme changes", async () => {
  const b = await browser({ storageFails: true, dark: false });
  assert.equal(b.applied(), "light");
  assert.doesNotThrow(() => b.theme.setThemePreference("dark"));
  assert.equal(b.applied(), "dark");
  b.theme.setThemePreference("system");
  b.system(true);
  assert.equal(b.applied(), "dark");
});
