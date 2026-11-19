import { ref, watch, type WatchSource } from "vue";

// Only result-scope sources belong here: changing pages must retain selection.
export function useAlbumSelection(scope: WatchSource<unknown>[]) {
  const selected = ref(new Set<number>());
  const selecting = ref(false);
  function clear() { selected.value = new Set(); }
  function toggle(id: number) {
    const next = new Set(selected.value);
    if (next.has(id)) next.delete(id); else next.add(id);
    selected.value = next;
  }
  function selectPage(ids: number[]) { selected.value = new Set([...selected.value, ...ids]); }
  function stop() { clear(); selecting.value = false; }
  watch(scope, clear, { flush: "sync" });
  return { selected, selecting, clear, toggle, selectPage, stop };
}
