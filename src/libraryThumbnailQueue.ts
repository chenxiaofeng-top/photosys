type LoadThumbnail = () => Promise<void>;

let maxActive = 1;
const waiting = new Set<LoadThumbnail>();
let active = 0;

function drain() {
  for (const load of waiting) {
    if (active >= maxActive) break;
    waiting.delete(load);
    active++;
    void load().finally(() => {
      active--;
      drain();
    });
  }
}

export function setLibraryThumbnailConcurrency(limit: number) {
  maxActive = limit;
}

export function queueLibraryThumbnail(load: LoadThumbnail) {
  waiting.add(load);
  drain();
  return () => { waiting.delete(load); };
}
