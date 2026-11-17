<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, ref, shallowRef, watch } from "vue";
import { formatBytes } from "./formatBytes";

interface DuplicateFile {
  id: number;
  name: string;
  relativePath: string;
  modifiedAt: string;
  hardLink: boolean;
}

interface DuplicateGroup {
  id: number;
  size: number;
  needsRescan: boolean;
  files: DuplicateFile[];
}

interface ScanResult {
  scanId: number;
  scanned: number;
  groups: DuplicateGroup[];
  issues: { path: string; reason: string }[];
}

type ScanProgress =
  | { phase: "scanning"; found: number }
  | { phase: "hashing"; checked: number; total: number; currentFile: string }
  | { phase: "comparing"; checked: number; total: number };

interface TrashOutcome {
  id: number;
  relativePath: string;
  status: "moved" | "skipped" | "failed";
  reason: string | null;
}

interface TrashResult {
  groups: DuplicateGroup[];
  outcomes: TrashOutcome[];
}

const emit = defineEmits<{ busyChange: [busy: boolean] }>();
const directory = ref("");
const task = ref<"choosing" | "scanning" | "trashing">();
const busy = computed(() => task.value !== undefined);
watch(busy, (value) => emit("busyChange", value), { flush: "sync" });

const scan = shallowRef<ScanResult>();
const progress = shallowRef<ScanProgress>();
const outcomes = shallowRef<TrashOutcome[]>([]);
const error = ref("");
const confirmation = ref<HTMLDialogElement>();
const selected = shallowRef(new Set<number>());

const pageSize = 25;
const page = ref(1);
const groups = computed(() => scan.value?.groups ?? []);
const pageCount = computed(() => Math.max(1, Math.ceil(groups.value.length / pageSize)));
const pageGroups = computed(() => groups.value.slice((page.value - 1) * pageSize, page.value * pageSize));
const canSelectExtras = computed(() => groups.value.some((group) =>
  !group.needsRescan && group.files.slice(1).some((file) => !file.hardLink),
));

const selectedGroups = computed(() => {
  if (!selected.value.size) return [];
  return groups.value.flatMap((group) => {
    const moving = group.files.filter((file) => selected.value.has(file.id));
    if (!moving.length) return [];
    return [{
      id: group.id,
      moving,
      keeping: group.files.filter((file) => !selected.value.has(file.id)),
      size: group.size * moving.length,
    }];
  });
});
const selectedByGroup = computed(() => new Map(selectedGroups.value.map((group) => [group.id, group])));
const selectedBytes = computed(() => selectedGroups.value.reduce((total, group) => total + group.size, 0));
const outcomeCounts = computed(() => {
  const counts = { moved: 0, skipped: 0, failed: 0 };
  for (const outcome of outcomes.value) counts[outcome.status]++;
  return counts;
});

const dateFormatter = new Intl.DateTimeFormat("zh-CN", { dateStyle: "short", timeStyle: "medium" });
const statusLabels = { moved: "已移入回收站", skipped: "已跳过", failed: "失败" };

function resetScan() {
  scan.value = undefined;
  selected.value = new Set();
  outcomes.value = [];
  page.value = 1;
}

async function chooseDirectory() {
  if (busy.value) return;
  task.value = "choosing";
  error.value = "";
  try {
    const folder = await invoke<string | null>("choose_folder", { title: "选择查重目录" });
    if (folder !== null && directory.value !== folder) {
      directory.value = folder;
      resetScan();
    }
  } catch (cause) {
    error.value = `无法选择目录。${String(cause)}`;
  } finally {
    task.value = undefined;
  }
}

async function scanDirectory() {
  if (!directory.value || busy.value) return;
  task.value = "scanning";
  error.value = "";
  resetScan();
  progress.value = { phase: "scanning", found: 0 };
  let receiving = true;
  try {
    const channel = new Channel<ScanProgress>((update) => {
      if (receiving) progress.value = update;
    });
    scan.value = await invoke<ScanResult>("scan_duplicates", { directory: directory.value, progress: channel });
  } catch (cause) {
    error.value = String(cause);
  } finally {
    receiving = false;
    progress.value = undefined;
    task.value = undefined;
  }
}

function selectionDisabled(group: DuplicateGroup, file: DuplicateFile) {
  return busy.value || group.needsRescan || file.hardLink ||
    (!selected.value.has(file.id) && selectedByGroup.value.get(group.id)?.keeping.length === 1);
}

function toggleFile(group: DuplicateGroup, file: DuplicateFile) {
  if (selectionDisabled(group, file)) return;
  const selection = new Set(selected.value);
  if (selection.has(file.id)) selection.delete(file.id);
  else selection.add(file.id);
  selected.value = selection;
}

function selectExtras() {
  const selection = new Set<number>();
  for (const group of groups.value) {
    if (group.needsRescan) continue;
    for (const file of group.files.slice(1)) {
      if (!file.hardLink) selection.add(file.id);
    }
  }
  selected.value = selection;
}

async function trashSelection() {
  if (!scan.value || busy.value || !selected.value.size || !confirmation.value?.open) return;
  const current = scan.value;
  const fileIds = [...selected.value];
  confirmation.value.close();
  task.value = "trashing";
  error.value = "";
  outcomes.value = [];
  try {
    const result = await invoke<TrashResult>("trash_duplicates", { scanId: current.scanId, fileIds });
    scan.value = { ...current, groups: result.groups };
    page.value = Math.min(page.value, pageCount.value);
    selected.value = new Set();
    outcomes.value = result.outcomes;
  } catch (cause) {
    error.value = String(cause);
  } finally {
    task.value = undefined;
  }
}
</script>

<template>
  <div>
    <form @submit.prevent="scanDirectory">
      <label for="duplicate-dir">扫描目录</label>
      <div class="folder-row">
        <input id="duplicate-dir" :value="directory" placeholder="尚未选择扫描目录" readonly />
        <button type="button" class="secondary" :disabled="busy" @click="chooseDirectory">选择目录</button>
      </div>
      <div class="actions">
        <button type="submit" :disabled="busy || !directory">
          {{ task === "scanning" ? "正在扫描…" : scan ? "重新扫描" : "开始扫描" }}
        </button>
      </div>
      <div v-if="progress" class="archive-progress" role="status">
        <template v-if="progress.phase === 'scanning'">
          <p>正在读取目录，已发现 {{ progress.found }} 个文件</p>
          <progress aria-label="读取目录进度"></progress>
        </template>
        <template v-else>
          <p>{{ progress.phase === "hashing" ? "正在检查文件" : "正在比对内容" }} {{ progress.checked }} / {{ progress.total }}</p>
          <progress v-if="progress.total" :value="progress.checked" :max="progress.total" aria-label="查重进度"></progress>
          <p v-if="progress.phase === 'hashing'" class="current-file">{{ progress.currentFile }}</p>
        </template>
      </div>
      <p v-if="task === 'trashing'" role="status">正在核验副本并移到回收站…</p>
      <p v-if="error" class="error" role="alert">{{ error }}</p>
    </form>

    <section v-if="scan" aria-labelledby="duplicates-results">
      <h2 id="duplicates-results">查重结果</h2>
      <dl aria-live="polite">
        <div><dt>扫描文件</dt><dd>{{ scan.scanned }}</dd></div>
        <div><dt>重复组</dt><dd>{{ groups.length }}</dd></div>
      </dl>
      <details v-if="scan.issues.length" class="duplicate-details">
        <summary>扫描时跳过 {{ scan.issues.length }} 项</summary>
        <ul class="duplicate-list">
          <li v-for="(issue, index) in scan.issues" :key="index">
            <strong>{{ issue.path }}</strong>
            <span>{{ issue.reason }}</span>
          </li>
        </ul>
      </details>

      <div v-if="outcomes.length" class="batch-result">
        <p role="status">
          已移入回收站 {{ outcomeCounts.moved }} 个，跳过 {{ outcomeCounts.skipped }} 个，失败 {{ outcomeCounts.failed }} 个。
        </p>
        <details class="duplicate-details" :open="outcomeCounts.skipped + outcomeCounts.failed > 0">
          <summary>处理明细</summary>
          <ul class="duplicate-list">
            <li v-for="outcome in outcomes" :key="outcome.id">
              <strong>{{ outcome.relativePath }}</strong>
              <span>{{ statusLabels[outcome.status] }}<template v-if="outcome.reason"> · {{ outcome.reason }}</template></span>
            </li>
          </ul>
        </details>
      </div>

      <p v-if="!groups.length">{{ outcomes.length ? "当前结果中已无可列出的重复组。" : "未发现内容完全相同的文件。" }}</p>
      <template v-else>
        <div class="actions">
          <button type="button" class="secondary" :disabled="busy || !canSelectExtras"
            aria-describedby="selection-rule" @click="selectExtras">
            一键勾选多余副本
          </button>
          <button type="button" class="secondary" :disabled="busy || !selected.size" @click="selected = new Set()">
            清除勾选
          </button>
          <button type="button" :disabled="busy || !selected.size" @click="confirmation?.showModal()">
            {{ task === "trashing" ? "正在移入回收站…" : "移到回收站" }}
          </button>
        </div>
        <p id="selection-rule" class="file-meta">一键勾选时，每组优先保留最靠近扫描目录根部的文件。</p>
        <p class="selection-total" role="status">已勾选 {{ selected.size }} 个文件，共 {{ formatBytes(selectedBytes) }}</p>

        <article v-for="group in pageGroups" :key="group.id" class="duplicate-group">
          <h3>{{ group.files.length }} 个文件 · 每个 {{ formatBytes(group.size) }}</h3>
          <p v-if="group.needsRescan" class="error">本组文件已变化，请重新扫描后处理。</p>
          <ul class="duplicate-list">
            <li v-for="file in group.files" :key="file.id">
              <label class="duplicate-file">
                <input type="checkbox" :checked="selected.has(file.id)" :disabled="selectionDisabled(group, file)"
                  @change="toggleFile(group, file)" />
                <span>
                  <strong>{{ file.name }}</strong>
                  <span class="file-path">{{ file.relativePath }}</span>
                  <span class="file-meta">
                    <time :datetime="file.modifiedAt">{{ dateFormatter.format(new Date(file.modifiedAt)) }}</time>
                    <template v-if="file.hardLink"> · 硬链接，保留</template>
                    <template v-else-if="selectedByGroup.get(group.id)?.keeping.length === 1 && !selected.has(file.id)">
                      · 本组保留项
                    </template>
                  </span>
                </span>
              </label>
            </li>
          </ul>
        </article>

        <nav v-if="pageCount > 1" class="actions" aria-label="查重结果分页">
          <button type="button" class="secondary" :disabled="page === 1" @click="page--">上一页</button>
          <span role="status">第 {{ page }} / {{ pageCount }} 页</span>
          <button type="button" class="secondary" :disabled="page === pageCount" @click="page++">下一页</button>
        </nav>
      </template>
    </section>

    <dialog ref="confirmation" class="trash-dialog" aria-labelledby="trash-heading" aria-describedby="trash-summary">
      <h2 id="trash-heading">确认移到回收站</h2>
      <p id="trash-summary">
        将移入 {{ selected.size }} 个文件，逻辑大小共 {{ formatBytes(selectedBytes) }}。可从系统回收站恢复。
      </p>
      <p class="current-file">扫描目录 {{ directory }}</p>
      <div class="trash-preview" tabindex="0" aria-label="待处理和保留的文件">
        <article v-for="group in selectedGroups" :key="group.id">
          <h3>将移入回收站</h3>
          <ul><li v-for="file in group.moving" :key="file.id">{{ file.relativePath }}</li></ul>
          <h3>保留</h3>
          <ul><li v-for="file in group.keeping" :key="file.id">{{ file.relativePath }}</li></ul>
        </article>
      </div>
      <div class="actions">
        <button type="button" class="secondary" autofocus @click="confirmation?.close()">取消</button>
        <button type="button" class="danger" @click="trashSelection">确认移到回收站</button>
      </div>
    </dialog>
  </div>
</template>

<style scoped>
.batch-result {
  border-block-start: 1px solid #c9d9d4;
  margin-top: 1rem;
}

.duplicate-details summary {
  cursor: pointer;
  color: #194f44;
}

.duplicate-group {
  margin-top: 0.875rem;
  padding: 0.875rem 1rem;
  border: 1px solid #c9d9d4;
  border-radius: 0.625rem;
}

.duplicate-group h3,
.trash-preview h3 {
  margin: 0;
  font-size: 1rem;
  overflow-wrap: anywhere;
}

.duplicate-list {
  display: grid;
  gap: 0.625rem;
  list-style: none;
  padding: 0;
  margin: 0.625rem 0 0;
}

.duplicate-list li > :is(strong, span),
.file-path,
.file-meta {
  display: block;
  overflow-wrap: anywhere;
}

.duplicate-file {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: 0.625rem;
  align-items: start;
  margin: 0;
  padding: 0.625rem 0 0;
  border-block-start: 1px solid #e1ebe7;
  font-weight: 400;
  cursor: pointer;
}

.duplicate-file:has(input:disabled) {
  cursor: default;
}

.duplicate-file input {
  width: 1rem;
  height: 1rem;
  margin: 0.15rem 0 0;
  accent-color: #176252;
}

.file-path,
.file-meta,
.selection-total {
  color: #395d57;
}

.file-meta {
  margin: 0.25rem 0 0;
  font-size: 0.875rem;
}

.trash-dialog {
  box-sizing: border-box;
  width: min(36rem, calc(100% - 2rem));
  max-height: calc(100% - 2rem);
  padding: clamp(1rem, 4vw, 1.5rem);
  border: 1px solid #a6c4bb;
  border-radius: 0.875rem;
  color: inherit;
}

.trash-dialog::backdrop {
  background: rgb(15 36 32 / 55%);
}

.trash-preview {
  max-height: 35vh;
  overflow: auto;
  overflow-wrap: anywhere;
  border-block: 1px solid #c9d9d4;
}

.trash-preview article {
  padding: 0.75rem 0.5rem;
}

.trash-preview article + article {
  border-block-start: 1px solid #c9d9d4;
}

.trash-preview ul {
  padding-inline-start: 1.5rem;
  margin: 0.25rem 0 0.5rem;
}

.trash-dialog .actions {
  justify-content: end;
}

.danger {
  background: #9c2626;
}
</style>
