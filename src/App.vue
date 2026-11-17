<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, ref } from "vue";
import DuplicatesPanel from "./DuplicatesPanel.vue";
import MediaLibrary from "./MediaLibrary.vue";
import SimilarPhotosPanel from "./SimilarPhotosPanel.vue";
import { formatBytes } from "./formatBytes";

interface ArchiveSummary {
  found: number;
  copied: number;
  duplicates: number;
  renamed: number;
  other: number;
  failed: number;
  scanErrors: number;
  reportPath: string | null;
  reportError: string | null;
}

interface CopyProgress {
  bytesWritten: number;
  totalBytes: number;
}

type ArchiveProgress =
  | { phase: "scanning"; found: number }
  | { phase: "archiving"; processed: number; total: number; currentFile: string; transfer: CopyProgress | null }
  | { phase: "reporting"; processed: number; total: number };

const source = ref("");
const destination = ref("");
const choosing = ref(false);
const running = ref(false);
const result = ref<ArchiveSummary>();
const progress = ref<ArchiveProgress>();
const error = ref("");
const viewTitles = {
  library: "媒体库", archive: "照片与视频归档", duplicates: "重复文件", similar: "相似照片",
};
type View = keyof typeof viewTitles;
const view = ref<View>("library");
const libraryDirectory = ref("");
const libraryBusy = ref(false);
const duplicatesBusy = ref(false);
const similarBusy = ref(false);
const duplicatesPanel = ref<InstanceType<typeof DuplicatesPanel>>();
const similarPanel = ref<InstanceType<typeof SimilarPhotosPanel>>();
const fileTaskBusy = computed(() =>
  libraryBusy.value || choosing.value || running.value || duplicatesBusy.value || similarBusy.value,
);

function navigate(target: View) {
  if (fileTaskBusy.value) return;
  const directory = libraryDirectory.value;
  if (view.value === "library" && directory) {
    if (target === "archive" && source.value !== directory) {
      source.value = directory;
      result.value = undefined;
      error.value = "";
    } else if (target === "duplicates") {
      duplicatesPanel.value?.useDirectory(directory);
    } else if (target === "similar") {
      similarPanel.value?.useDirectory(directory);
    }
  }
  view.value = target;
}

async function chooseFolder(kind: "source" | "destination") {
  if (choosing.value || running.value) return;
  choosing.value = true;
  error.value = "";

  try {
    const folder = await invoke<string | null>("choose_folder", {
      title: kind === "source" ? "选择源目录" : "选择目标目录",
    });
    if (folder !== null) {
      const selected = kind === "source" ? source : destination;
      if (selected.value !== folder) {
        selected.value = folder;
        result.value = undefined;
      }
    }
  } catch (cause) {
    error.value = `无法选择目录。${String(cause)}`;
  } finally {
    choosing.value = false;
  }
}

async function startArchive() {
  if (!source.value || !destination.value || choosing.value || running.value) return;
  running.value = true;
  error.value = "";
  result.value = undefined;
  progress.value = { phase: "scanning", found: 0 };
  let receivingProgress = true;

  try {
    const channel = new Channel<ArchiveProgress>((update) => {
      if (receivingProgress) progress.value = update;
    });
    result.value = await invoke<ArchiveSummary>("archive_media", {
      source: source.value,
      destination: destination.value,
      progress: channel,
    });
  } catch (cause) {
    error.value = String(cause);
  } finally {
    receivingProgress = false;
    progress.value = undefined;
    running.value = false;
  }
}
</script>

<template>
  <main :class="{ 'library-layout': view === 'library' }">
    <header>
      <p class="app-name">PhotoSys</p>
      <nav class="view-nav" aria-label="功能">
        <button v-for="(title, target) in viewTitles" :key="target" type="button" class="secondary"
          :aria-pressed="view === target" :disabled="fileTaskBusy" @click="navigate(target)">
          {{ title }}
        </button>
      </nav>
      <h1>{{ viewTitles[view] }}</h1>
      <p v-if="view === 'archive'" class="description">复制源目录及其子目录中的媒体，按年份和月份整理到目标目录。</p>
    </header>

    <MediaLibrary v-show="view === 'library'" @busy-change="libraryBusy = $event"
      @directory-change="libraryDirectory = $event" />

    <form v-if="view === 'archive'" @submit.prevent="startArchive">
      <div class="folder">
        <label for="source">源目录</label>
        <div class="folder-row">
          <input id="source" :value="source" placeholder="尚未选择源目录" readonly />
          <button type="button" class="secondary" :disabled="choosing || running" @click="chooseFolder('source')">
            选择源目录
          </button>
        </div>
      </div>

      <div class="folder">
        <label for="destination">目标目录</label>
        <div class="folder-row">
          <input id="destination" :value="destination" placeholder="尚未选择目标目录" readonly />
          <button type="button" class="secondary" :disabled="choosing || running" @click="chooseFolder('destination')">
            选择目标目录
          </button>
        </div>
      </div>

      <div class="actions">
        <button type="submit" :disabled="!source || !destination || choosing || running">
          {{ running ? "正在复制…" : "开始复制" }}
        </button>
      </div>
      <div v-if="progress" class="archive-progress">
        <template v-if="progress.phase === 'scanning'">
          <p role="status">正在扫描媒体文件…</p>
          <progress aria-label="扫描进度"></progress>
          <p>已发现 {{ progress.found }} 个文件</p>
        </template>
        <template v-else>
          <template v-if="progress.total > 0">
            <p>已处理 {{ progress.processed }} / {{ progress.total }} 个文件（{{ Math.floor(progress.processed / progress.total * 100) }}%）</p>
            <progress aria-label="总体归档进度" :value="progress.processed" :max="progress.total"></progress>
          </template>
          <p v-if="progress.phase === 'reporting'" role="status">正在生成复制说明…</p>
          <template v-else>
            <p class="current-file">正在处理 {{ progress.currentFile }}</p>
            <template v-if="progress.transfer && progress.transfer.totalBytes > 0">
              <progress aria-label="当前文件复制进度" :value="progress.transfer.bytesWritten" :max="progress.transfer.totalBytes"></progress>
              <p>{{ formatBytes(progress.transfer.bytesWritten) }} / {{ formatBytes(progress.transfer.totalBytes) }}</p>
            </template>
          </template>
        </template>
      </div>
      <p v-if="error" class="error" role="alert">{{ error }}</p>
    </form>

    <section v-if="view === 'archive' && result" aria-labelledby="archive-result" role="status">
      <h2 id="archive-result">
        {{ result.failed || result.scanErrors || result.reportError ? "归档结束，有错误" : "归档完成" }}
      </h2>
      <dl>
        <div><dt>发现媒体</dt><dd>{{ result.found }}</dd></div>
        <div><dt>成功复制</dt><dd>{{ result.copied }}</dd></div>
        <div><dt>重复跳过</dt><dd>{{ result.duplicates }}</dd></div>
        <div><dt>重命名</dt><dd>{{ result.renamed }}</dd></div>
        <div><dt>归入其它</dt><dd>{{ result.other }}</dd></div>
        <div><dt>复制失败</dt><dd>{{ result.failed }}</dd></div>
        <div v-if="result.scanErrors"><dt>扫描错误</dt><dd>{{ result.scanErrors }}</dd></div>
      </dl>
      <p v-if="result.reportPath" class="report">复制说明：<span>{{ result.reportPath }}</span></p>
      <p v-if="result.reportError" class="error">无法写入复制说明。{{ result.reportError }}</p>
    </section>
    <DuplicatesPanel v-show="view === 'duplicates'" ref="duplicatesPanel" @busy-change="duplicatesBusy = $event" />
    <SimilarPhotosPanel v-show="view === 'similar'" ref="similarPanel" @busy-change="similarBusy = $event" />
  </main>
</template>

<style>
:root {
  font-family: Inter, system-ui, sans-serif;
  color: #152e30;
  background: #eef4f1;
}

body {
  margin: 0;
}

main {
  max-width: 42rem;
  margin: 0 auto;
  padding: clamp(1.25rem, 6vw, 3.5rem);
}

main.library-layout {
  max-width: 72rem;
  padding: clamp(1rem, 4vw, 2.5rem);
}

header {
  margin-bottom: 1.5rem;
}

.app-name {
  margin: 0 0 0.5rem;
  color: #366560;
  font-weight: 700;
}

.view-nav {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
  margin-bottom: 1rem;
}

.view-nav button[aria-pressed="true"]:not(:disabled) {
  border-color: #176252;
  background: #176252;
  color: #fff;
}

h1 {
  margin: 0;
  font-size: 2rem;
}

h2 {
  margin: 0;
  font-size: 1.25rem;
}

.description {
  margin: 0.5rem 0 0;
  color: #395d57;
}

form,
section {
  padding: clamp(1rem, 4vw, 1.75rem);
  background: #fff;
  border: 1px solid #c9d9d4;
  border-radius: 0.875rem;
}

section,
.folder + .folder {
  margin-top: 1rem;
}

label {
  display: block;
  margin-bottom: 0.5rem;
  font-weight: 650;
}

.folder-row,
.actions {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem;
}

.folder-row input {
  flex: 1 1 14rem;
  min-width: 0;
  padding: 0.625rem 0.75rem;
  border: 1px solid #829d95;
  border-radius: 0.5rem;
  background: #fcfefd;
  color: inherit;
  font: inherit;
}

.actions {
  align-items: center;
  gap: 0.75rem;
  margin-top: 1.25rem;
}

.archive-progress {
  display: grid;
  gap: 0.375rem;
  margin-top: 1rem;
  color: #395d57;
  font-variant-numeric: tabular-nums;
}

.archive-progress p {
  margin: 0;
}

.archive-progress progress {
  width: 100%;
  accent-color: #176252;
}

.current-file {
  overflow-wrap: anywhere;
}

.report {
  margin: 0;
  color: #395d57;
}

.error {
  margin: 0.75rem 0 0;
  color: #9c2626;
  overflow-wrap: anywhere;
}

button {
  min-height: 2.5rem;
  padding: 0.5rem 1rem;
  border: 1px solid transparent;
  border-radius: 0.5rem;
  background: #176252;
  color: #fff;
  font: inherit;
  font-weight: 650;
  cursor: pointer;
}

button.secondary {
  border-color: #a6c4bb;
  background: #f0f7f4;
  color: #194f44;
}

button:disabled {
  background: #dde8e3;
  color: #536863;
  cursor: default;
}

:is(button, input):focus-visible {
  outline: 2px solid #126353;
  outline-offset: 2px;
}

dl {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(5rem, 1fr));
  gap: 0.75rem;
  margin: 1rem 0;
}

dt {
  color: #395d57;
  font-size: 0.875rem;
}

dd {
  margin: 0.25rem 0 0;
  font-size: 1.25rem;
  font-weight: 650;
  font-variant-numeric: tabular-nums;
}

.report span {
  overflow-wrap: anywhere;
}
</style>
