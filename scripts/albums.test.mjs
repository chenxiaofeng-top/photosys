import assert from "node:assert/strict";
import { test } from "node:test";
import { effectScope, ref } from "vue";
import { useAlbumSelection } from "../src/albumSelection.ts";
import { validAlbumName } from "../src/albums.ts";

test("selection retains pages, deduplicates page selections and toggles individually", () => {
  const context = ref(1);
  const scope = effectScope();
  const state = scope.run(() => useAlbumSelection([context]));
  state.selecting.value = true;
  state.selectPage([0, 1, 2]);
  state.selectPage([2, 24, 25]);
  state.toggle(1);
  assert.deepEqual([...state.selected.value], [0, 2, 24, 25]);
  state.toggle(1);
  assert.equal(state.selected.value.size, 5);
  state.clear();
  assert.equal(state.selected.value.size, 0);
  assert.equal(state.selecting.value, true);
  state.stop();
  assert.equal(state.selecting.value, false);
  scope.stop();
});

test("scan, filters, sort, search mode, view and album changes clear scan-local selection immediately", () => {
  const sources = [ref(1), ref({ query: "" }), ref("dateDesc"), ref("filename"), ref("library"), ref(1)];
  const scope = effectScope();
  const state = scope.run(() => useAlbumSelection(sources));
  for (let i = 0; i < sources.length; i++) {
    state.selectPage([1, 2]);
    sources[i].value = i === 1 ? { query: "旅行" } : i < 2 || i === 5 ? 2 : "changed";
    assert.equal(state.selected.value.size, 0);
  }
  scope.stop();
});

test("album names trim whitespace and count Unicode code points", () => {
  assert.equal(validAlbumName("  周末露营  "), true);
  assert.equal(validAlbumName(" \n "), false);
  assert.equal(validAlbumName("字".repeat(81)), false);
  assert.equal(validAlbumName("😀".repeat(80)), true);
  assert.equal(validAlbumName("😀".repeat(81)), false);
});
