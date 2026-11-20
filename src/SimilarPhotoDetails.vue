<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef } from "vue";
import MediaDetailsInfo from "./MediaDetailsInfo.vue";
import type { Item, MediaDetails, MediaDimensions } from "./libraryTypes";

const props = defineProps<{
  scanId: number;
  photo: Pick<Item, "id" | "name" | "relativePath" | "size"> & MediaDimensions & { capturedAt: string | null };
  matchLabel: string;
}>();
const emit = defineEmits<{ close: [] }>();

const zoomLevels = [1, 1.5, 2, 3];
const dialog = ref<HTMLDialogElement>();
const viewport = ref<HTMLDivElement>();
const image = ref<HTMLImageElement>();
const viewportSize = ref({ width: 0, height: 0 });
const naturalSize = shallowRef<MediaDimensions>();
const zoomIndex = ref(0);
const zoom = computed(() => zoomLevels[zoomIndex.value]);
const fileInfo = computed(() => ({
  name: props.photo.name,
  size: props.photo.size,
  capturedOn: props.photo.capturedAt,
}));
const imageStyle = computed(() => {
  const size = naturalSize.value;
  const frame = viewportSize.value;
  if (!size || !frame.width || !frame.height) return { width: "0px", height: "0px" };
  const scale = Math.min(frame.width / size.width, frame.height / size.height) * zoom.value;
  return {
    width: `${Math.max(1, Math.floor(size.width * scale))}px`,
    height: `${Math.max(1, Math.floor(size.height * scale))}px`,
  };
});

const source = ref("");
const previewLoading = ref(true);
const previewError = ref("");
const details = shallowRef<MediaDetails>();
const detailsLoading = ref(true);
const detailsError = ref("");
let active = true;
let observer: ResizeObserver | undefined;
let measureFrame: number | undefined;

async function loadPreview() {
  source.value = "";
  naturalSize.value = undefined;
  previewError.value = "";
  previewLoading.value = true;
  zoomIndex.value = 0;
  try {
    const result = await invoke<string>("similar_preview", {
      scanId: props.scanId, fileId: props.photo.id, detail: true, large: true,
    });
    if (active) source.value = result;
  } catch (cause) {
    if (active) {
      previewError.value = String(cause);
      previewLoading.value = false;
    }
  }
}

async function loadDetails() {
  detailsError.value = "";
  detailsLoading.value = true;
  try {
    const result = await invoke<MediaDetails>("similar_media_details", { scanId: props.scanId, fileId: props.photo.id });
    if (active) details.value = result;
  } catch (cause) {
    if (active) detailsError.value = String(cause);
  } finally {
    if (active) detailsLoading.value = false;
  }
}

function measureViewport() {
  const frame = viewport.value;
  if (!frame) return;
  const { clientWidth: width, clientHeight: height } = frame;
  if (width !== viewportSize.value.width || height !== viewportSize.value.height) {
    viewportSize.value = { width, height };
  }
}

function imageLoaded(event: Event) {
  const target = event.currentTarget as HTMLImageElement;
  naturalSize.value = { width: target.naturalWidth, height: target.naturalHeight };
  previewLoading.value = false;
  measureViewport();
}

function imageFailed() {
  source.value = "";
  naturalSize.value = undefined;
  previewError.value = "无法显示照片预览。";
  previewLoading.value = false;
}

async function changeZoom(index: number) {
  const frame = viewport.value;
  const photo = image.value;
  if (!frame || !photo?.offsetWidth || !photo.offsetHeight || index < 0 || index >= zoomLevels.length) return;

  const centerX = (frame.scrollLeft + frame.clientWidth / 2 - photo.offsetLeft) / photo.offsetWidth;
  const centerY = (frame.scrollTop + frame.clientHeight / 2 - photo.offsetTop) / photo.offsetHeight;
  zoomIndex.value = index;
  await nextTick();
  if (!active) return;
  frame.scrollLeft = photo.offsetLeft + centerX * photo.offsetWidth - frame.clientWidth / 2;
  frame.scrollTop = photo.offsetTop + centerY * photo.offsetHeight - frame.clientHeight / 2;
}

function closeViewer() {
  active = false;
  dialog.value?.close();
  emit("close");
}

onMounted(() => {
  dialog.value?.showModal();
  if (viewport.value) {
    observer = new ResizeObserver(() => {
      if (measureFrame !== undefined) return;
      measureFrame = requestAnimationFrame(() => {
        measureFrame = undefined;
        measureViewport();
      });
    });
    observer.observe(viewport.value);
    measureViewport();
  }
  void loadPreview();
  void loadDetails();
});

onBeforeUnmount(() => {
  active = false;
  observer?.disconnect();
  if (measureFrame !== undefined) cancelAnimationFrame(measureFrame);
  if (dialog.value?.open) dialog.value.close();
});
</script>

<template>
  <dialog ref="dialog" class="photo-dialog" aria-labelledby="similar-photo-title" @cancel.prevent="closeViewer">
    <header class="photo-header">
      <h2 id="similar-photo-title" :title="photo.name">{{ photo.name }}</h2>
      <button type="button" class="secondary" autofocus @click="closeViewer">关闭</button>
    </header>
    <div class="photo-body">
      <div class="photo-viewer">
        <div class="zoom-controls" role="group" aria-label="照片缩放">
          <button type="button" class="secondary" :disabled="!naturalSize || zoomIndex === 0"
            @click="changeZoom(zoomIndex - 1)">缩小</button>
          <output aria-label="相对适应窗口的倍率">{{ zoom }}×</output>
          <button type="button" class="secondary" :disabled="!naturalSize || zoomIndex === zoomLevels.length - 1"
            @click="changeZoom(zoomIndex + 1)">放大</button>
          <button type="button" class="secondary" :disabled="!naturalSize || zoomIndex === 0"
            @click="changeZoom(0)">适应窗口</button>
        </div>
        <div ref="viewport" class="photo-viewport" role="region" aria-label="照片预览" tabindex="0">
          <div v-if="previewLoading || previewError" class="preview-state">
            <p v-if="previewLoading" role="status">正在加载大图…</p>
            <template v-else>
              <p class="preview-error" role="alert">{{ previewError }}</p>
              <button type="button" class="secondary" @click="loadPreview">重试预览</button>
            </template>
          </div>
          <div v-if="source" class="pan-area">
            <img ref="image" :src="source" :alt="photo.name" :style="imageStyle" draggable="false"
              @load="imageLoaded" @error="imageFailed" />
          </div>
        </div>
      </div>
      <section class="photo-information" aria-labelledby="similar-photo-info">
        <h3 id="similar-photo-info">照片详情</h3>
        <dl class="comparison-info">
          <dt>相对路径</dt><dd>{{ photo.relativePath }}</dd>
          <dt>相似类型</dt><dd>{{ matchLabel }}</dd>
        </dl>
        <MediaDetailsInfo :item="fileInfo" :details="details" :photo-dimensions="photo"
          :loading="detailsLoading" :error="detailsError" />
        <button v-if="detailsError" type="button" class="secondary" @click="loadDetails">重试详情</button>
      </section>
    </div>
  </dialog>
</template>

<style scoped>
.photo-dialog {
  width: min(82rem, calc(100vw - 2rem));
  max-height: calc(100dvh - 2rem);
  padding: 0;
  overflow: hidden;
}

.photo-dialog[open] {
  display: flex;
  flex-direction: column;
}

.photo-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.75rem;
  padding: 0.75rem 1.25rem;
  border-bottom: 1px solid var(--border);
}

.photo-header h2 {
  margin: 0;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.photo-header button {
  flex: none;
}

.photo-body {
  display: grid;
  grid-template-columns: minmax(0, 1fr) clamp(16rem, 25vw, 20rem);
  height: min(72dvh, 50rem);
  min-height: 0;
}

.photo-viewer {
  display: grid;
  grid-template-rows: auto minmax(0, 1fr);
  min-width: 0;
  min-height: 0;
}

.zoom-controls {
  display: flex;
  flex-wrap: wrap;
  justify-content: center;
  align-items: center;
  gap: 0.5rem;
  padding: 0.5rem;
  background: var(--surface-subtle);
  border-bottom: 1px solid var(--border);
}

.zoom-controls button {
  padding-inline: 0.75rem;
}

.zoom-controls output {
  min-width: 3ch;
  color: var(--text-secondary);
  text-align: center;
  font-variant-numeric: tabular-nums;
}

.photo-viewport {
  position: relative;
  min-height: 0;
  overflow: auto;
  background: var(--viewer);
  color: var(--viewer-text);
  outline-offset: -3px;
}

.pan-area {
  display: grid;
  place-items: center;
  width: max-content;
  min-width: 100%;
  min-height: 100%;
}

.pan-area img {
  display: block;
  max-width: none;
}

.preview-state {
  position: absolute;
  z-index: 1;
  inset: 0;
  display: flex;
  flex-direction: column;
  justify-content: center;
  align-items: center;
  gap: 0.75rem;
  padding: 1rem;
  text-align: center;
  overflow-wrap: anywhere;
}

.preview-state p {
  margin: 0;
}

.preview-error {
  color: var(--viewer-error);
}

.photo-information {
  min-width: 0;
  overflow-y: auto;
  padding: 1rem 1.25rem;
  border-left: 1px solid var(--border);
}

.photo-information h3 {
  margin: 0 0 0.75rem;
}

.comparison-info {
  display: grid;
  grid-template-columns: 6.6em minmax(0, 1fr);
  gap: 0.4rem 0.6rem;
  margin: 0 0 0.85rem;
  font-size: 13px;
}

.comparison-info dd {
  margin: 0;
  font-size: inherit;
  overflow-wrap: anywhere;
}

@media (max-width: 47rem) {
  .photo-body {
    display: block;
    height: auto;
    overflow-y: auto;
  }

  .photo-viewer {
    height: clamp(12rem, 52dvh, 30rem);
  }

  .photo-information {
    overflow: visible;
    border-top: 1px solid var(--border);
    border-left: 0;
  }
}
</style>
