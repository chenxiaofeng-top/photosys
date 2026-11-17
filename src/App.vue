<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { ref } from "vue";

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

const source = ref("");
const destination = ref("");
const choosing = ref(false);
const running = ref(false);
const result = ref<ArchiveSummary>();
const error = ref("");

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

  try {
    result.value = await invoke<ArchiveSummary>("archive_media", {
      source: source.value,
      destination: destination.value,
    });
  } catch (cause) {
    error.value = String(cause);
  } finally {
    running.value = false;
  }
}
</script>

<template>
  <main>
    <header>
      <p class="app-name">PhotoSys</p>
      <h1>照片与视频归档</h1>
      <p class="description">复制源目录及其子目录中的媒体，按年份和月份整理到目标目录。</p>
    </header>

    <form @submit.prevent="startArchive">
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
        <p v-if="running" role="status">正在扫描并复制媒体文件。</p>
      </div>
      <p v-if="error" class="error" role="alert">{{ error }}</p>
    </form>

    <section v-if="result" aria-labelledby="archive-result" role="status">
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

header {
  margin-bottom: 1.5rem;
}

.app-name {
  margin: 0 0 0.5rem;
  color: #366560;
  font-weight: 700;
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

.actions p,
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
