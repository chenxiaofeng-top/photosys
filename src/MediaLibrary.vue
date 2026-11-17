<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, nextTick, onMounted, ref, shallowRef, watch } from "vue";
import LibraryThumbnail from "./LibraryThumbnail.vue";
import { formatBytes } from "./formatBytes";

type Kind = "photo" | "video";

interface Item {
  id: number;
  name: string;
  relativePath: string;
  size: number;
  kind: Kind;
  capturedOn: string | null;
  previewAvailable: boolean;
  favorite: boolean;
}

interface Page<T> {
  page: number;
  total: number;
  items: T[];
}

interface Issue {
  path: string;
  reason: string;
}

interface Summary {
  scanId: number;
  total: number;
  issues: number;
  favoritesWarning: string | null;
}

const emit = defineEmits<{ busyChange: [busy: boolean]; directoryChange: [directory: string] }>();
const pageSize = 24;
const directory = ref("");
const task = ref<"starting" | "choosing" | "scanning" | "saving" | undefined>("starting");
const busy = computed(() => task.value !== undefined);
watch(busy, (value) => emit("busyChange", value), { flush: "sync", immediate: true });

const summary = shallowRef<Summary>();
const found = ref(0);
const scanError = ref("");
const favoriteError = ref("");
const itemsError = ref("");
const issuesError = ref("");

const kind = ref<Kind | "all">("all");
const dateMode = ref<"all" | "range" | "unknown">("all");
const from = ref("");
const to = ref("");
const favoritesOnly = ref(false);
const filter = computed(() => ({
  kind: kind.value === "all" ? null : kind.value,
  from: dateMode.value === "range" ? from.value || null : null,
  to: dateMode.value === "range" ? to.value || null : null,
  unknownDate: dateMode.value === "unknown",
  favoritesOnly: favoritesOnly.value,
}));

const requestedPage = ref(0);
const revision = ref(0);
const results = shallowRef<Page<Item>>();
watch(filter, () => { requestedPage.value = 0; }, { flush: "sync" });
watch([summary, filter, requestedPage, revision], async ([scan, currentFilter, page], _, onCleanup) => {
  if (!scan) return;
  let receiving = true;
  onCleanup(() => { receiving = false; });
  itemsError.value = "";
  try {
    const result = await invoke<Page<Item>>("library_items", { scanId: scan.scanId, filter: currentFilter, page });
    if (receiving) results.value = result;
  } catch (cause) {
    if (receiving) {
      results.value = undefined;
      itemsError.value = String(cause);
    }
  }
});

const issuesOpen = ref(false);
const issuePage = ref(0);
const issues = shallowRef<Page<Issue>>();
watch([summary, issuesOpen, issuePage], async ([scan, open, page], _, onCleanup) => {
  if (!scan?.issues || !open) return;
  let receiving = true;
  onCleanup(() => { receiving = false; });
  issuesError.value = "";
  try {
    const result = await invoke<Page<Issue>>("library_issues", { scanId: scan.scanId, page });
    if (receiving) issues.value = result;
  } catch (cause) {
    if (receiving) issuesError.value = String(cause);
  }
});

const detail = shallowRef<Item>();
const dialog = ref<HTMLDialogElement>();
const fullImage = ref("");
const detailError = ref("");
const opening = ref(false);

onMounted(async () => {
  try {
    const initial = await invoke<string | null>("library_startup_directory");
    if (initial) {
      await scanDirectory(initial);
      return;
    }
  } catch (cause) {
    scanError.value = String(cause);
  } finally {
    task.value = undefined;
  }
});

async function chooseDirectory() {
  if (busy.value) return;
  task.value = "choosing";
  scanError.value = "";
  try {
    const selected = await invoke<string | null>("choose_folder", { title: "选择媒体库目录" });
    if (selected) await scanDirectory(selected);
  } catch (cause) {
    scanError.value = `无法选择目录。${String(cause)}`;
  } finally {
    task.value = undefined;
  }
}

async function scanDirectory(selected: string) {
  task.value = "scanning";
  scanError.value = "";
  favoriteError.value = "";
  found.value = 0;
  try {
    const progress = new Channel<{ found: number }>((update) => {
      if (task.value === "scanning") found.value = update.found;
    });
    const scanned = await invoke<Summary>("scan_library", { directory: selected, progress });
    if (selected !== directory.value) {
      kind.value = "all";
      dateMode.value = "all";
      from.value = "";
      to.value = "";
      favoritesOnly.value = false;
    }
    directory.value = selected;
    emit("directoryChange", selected);
    results.value = undefined;
    issues.value = undefined;
    issuesOpen.value = false;
    issuePage.value = 0;
    requestedPage.value = 0;
    summary.value = scanned;
  } catch (cause) {
    scanError.value = String(cause);
  } finally {
    task.value = undefined;
  }
}

async function toggleFavorite(item: Item) {
  const scanId = summary.value?.scanId;
  if (scanId === undefined || busy.value) return;
  task.value = "saving";
  favoriteError.value = "";
  try {
    await invoke("set_library_favorite", { scanId, fileId: item.id, favorite: !item.favorite });
    const page = results.value;
    if (page) {
      results.value = {
        ...page,
        items: page.items.map((current) => current.id === item.id ? { ...current, favorite: !item.favorite } : current),
      };
    }
    revision.value++;
  } catch (cause) {
    favoriteError.value = String(cause);
  } finally {
    task.value = undefined;
  }
}

async function showDetails(item: Item) {
  const scanId = summary.value?.scanId;
  if (scanId === undefined || busy.value) return;
  detail.value = item;
  fullImage.value = "";
  detailError.value = "";
  opening.value = false;
  await nextTick();
  dialog.value?.showModal();
  if (!item.previewAvailable) return;
  try {
    const image = await invoke<string>("library_preview", { scanId, fileId: item.id, detail: true });
    if (detail.value === item) fullImage.value = image;
  } catch (cause) {
    if (detail.value === item) detailError.value = `无法预览。${String(cause)}`;
  }
}

async function openItem() {
  const item = detail.value;
  const scanId = summary.value?.scanId;
  if (!item || scanId === undefined || opening.value) return;
  opening.value = true;
  detailError.value = "";
  try {
    await invoke("open_library_item", { scanId, fileId: item.id });
  } catch (cause) {
    if (detail.value === item) detailError.value = String(cause);
  } finally {
    if (detail.value === item) opening.value = false;
  }
}
</script>

<template>
  <section class="media-library" aria-label="媒体库">
    <label for="library-directory">媒体库目录</label>
    <div class="folder-row">
      <input id="library-directory" :value="directory" placeholder="尚未选择目录" readonly />
      <button type="button" class="secondary" :disabled="busy" @click="chooseDirectory">选择目录</button>
      <button v-if="directory" type="button" class="secondary" :disabled="busy" @click="scanDirectory(directory)">刷新</button>
    </div>
    <p v-if="task === 'scanning'" class="status" role="status">正在扫描，已发现 {{ found }} 个媒体文件…</p>
    <p v-if="scanError" class="error" role="alert">{{ scanError }}</p>

    <template v-if="summary">
      <div class="filters">
        <label>类型
          <select v-model="kind">
            <option value="all">全部类型</option>
            <option value="photo">照片</option>
            <option value="video">视频</option>
          </select>
        </label>
        <label>拍摄日期
          <select v-model="dateMode">
            <option value="all">全部日期</option>
            <option value="range">日期范围</option>
            <option value="unknown">日期未知</option>
          </select>
        </label>
        <label v-if="dateMode === 'range'">起始日期<input v-model="from" type="date" /></label>
        <label v-if="dateMode === 'range'">结束日期<input v-model="to" type="date" /></label>
        <label class="favorite-filter"><input v-model="favoritesOnly" type="checkbox" /> 仅收藏</label>
      </div>

      <p v-if="summary.favoritesWarning" class="error" role="alert">{{ summary.favoritesWarning }}</p>
      <p v-if="favoriteError" class="error" role="alert">{{ favoriteError }}</p>
      <p v-if="itemsError" class="error" role="alert">{{ itemsError }}</p>
      <p v-else-if="results" class="status" role="status">
        {{ results.total ? `显示 ${results.total} / ${summary.total} 个媒体文件`
          : summary.total ? "没有符合筛选条件的媒体文件。" : "此目录没有可识别的照片或视频。" }}
      </p>

      <ul v-if="results?.items.length" class="media-grid">
        <li v-for="item in results.items" :key="`${summary.scanId}:${item.id}`" class="card">
          <button type="button" class="media-preview secondary" :disabled="busy"
            :aria-label="`查看 ${item.name}`" @click="showDetails(item)">
            <LibraryThumbnail v-if="item.previewAvailable" :scan-id="summary.scanId" :file-id="item.id" />
            <span v-else>{{ item.kind === "video" ? "▶ 视频" : "照片" }}</span>
          </button>
          <div class="card-details">
            <p class="file-name" :title="item.relativePath">{{ item.name }}</p>
            <div class="card-info">
              <span>{{ item.capturedOn ?? "日期未知" }}</span>
              <button type="button" class="favorite secondary" :aria-label="`收藏 ${item.name}`"
                :title="item.favorite ? '取消收藏' : '收藏'" :aria-pressed="item.favorite"
                :disabled="busy" @click="toggleFavorite(item)">{{ item.favorite ? "★" : "☆" }}</button>
            </div>
          </div>
        </li>
      </ul>
      <nav v-if="results && results.total > pageSize" class="pagination" aria-label="媒体列表分页">
        <button type="button" class="secondary" :disabled="busy || results.page === 0"
          @click="requestedPage = results.page - 1">上一页</button>
        <span>{{ results.page + 1 }} / {{ Math.ceil(results.total / pageSize) }}</span>
        <button type="button" class="secondary" :disabled="busy || (results.page + 1) * pageSize >= results.total"
          @click="requestedPage = results.page + 1">下一页</button>
      </nav>

      <details v-if="summary.issues" :open="issuesOpen" @toggle="issuesOpen = ($event.target as HTMLDetailsElement).open">
        <summary>扫描问题（{{ summary.issues }}）</summary>
        <p v-if="issuesError" class="error" role="alert">{{ issuesError }}</p>
        <ul v-if="issues">
          <li v-for="(issue, index) in issues.items" :key="index">{{ issue.path }} · {{ issue.reason }}</li>
        </ul>
        <nav v-if="issues && issues.total > pageSize" class="pagination" aria-label="扫描问题分页">
          <button type="button" class="secondary" :disabled="issues.page === 0" @click="issuePage = issues.page - 1">上一页</button>
          <span>{{ issues.page + 1 }} / {{ Math.ceil(issues.total / pageSize) }}</span>
          <button type="button" class="secondary" :disabled="(issues.page + 1) * pageSize >= issues.total"
            @click="issuePage = issues.page + 1">下一页</button>
        </nav>
      </details>
    </template>

    <dialog v-if="detail" ref="dialog" aria-labelledby="library-preview-title" @close="detail = undefined">
      <header class="detail-header">
        <h2 id="library-preview-title">{{ detail.name }}</h2>
        <button type="button" class="secondary" @click="dialog?.close()">关闭</button>
      </header>
      <img v-if="fullImage" class="detail-image" :src="fullImage" :alt="detail.name" />
      <p v-else-if="detail.previewAvailable && !detailError" class="status" role="status">正在加载预览…</p>
      <p class="detail-path">{{ detail.relativePath }}</p>
      <p class="status">{{ detail.capturedOn ?? "日期未知" }} · {{ formatBytes(detail.size) }}</p>
      <button type="button" :disabled="opening" @click="openItem">
        {{ detail.kind === "video" ? "用系统播放器打开" : "用系统默认应用打开" }}
      </button>
      <p v-if="detailError" class="error" role="alert">{{ detailError }}</p>
    </dialog>
  </section>
</template>

<style scoped>
.filters {
  display: flex;
  flex-wrap: wrap;
  align-items: end;
  gap: 0.75rem;
  margin-top: 1.25rem;
}

.filters label {
  display: grid;
  gap: 0.375rem;
  margin: 0;
  font-size: 0.875rem;
}

.filters :is(select, input[type="date"]) {
  padding: 0.5rem;
  min-height: 2.5rem;
  box-sizing: border-box;
  border: 1px solid #829d95;
  border-radius: 0.5rem;
  background: #fcfefd;
  color: inherit;
  font: inherit;
}

.filters select:focus-visible {
  outline: 2px solid #126353;
  outline-offset: 2px;
}

.filters .favorite-filter {
  display: flex;
  align-items: center;
  min-height: 2.5rem;
}

.favorite-filter input {
  accent-color: #176252;
}

.status {
  color: #395d57;
}

.media-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(min(10rem, 100%), 1fr));
  gap: 0.875rem;
  margin: 0;
  padding: 0;
  list-style: none;
}

.card {
  min-width: 0;
  border: 1px solid #c9d9d4;
  border-radius: 0.75rem;
  background: #fcfefd;
}

button.media-preview {
  display: grid;
  width: 100%;
  aspect-ratio: 4 / 3;
  grid-template: minmax(0, 1fr) / minmax(0, 1fr);
  place-items: center;
  padding: 0;
  overflow: hidden;
  border: 0;
  border-radius: 0.7rem 0.7rem 0 0;
  background: #edf4f1;
}

.card-details {
  padding: 0.625rem 0.75rem 0.375rem;
}

.file-name {
  margin: 0;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
  font-weight: 650;
}

.card-info {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 0.25rem;
  color: #395d57;
  font-size: 0.875rem;
}

button.favorite {
  min-width: 2.5rem;
  padding: 0;
  border-color: transparent;
  background: transparent;
  font-size: 1.375rem;
}

button.favorite[aria-pressed="true"] {
  color: #865000;
}

.pagination,
.detail-header {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.75rem;
}

.pagination {
  margin-top: 1rem;
}

details {
  margin-top: 1rem;
  overflow-wrap: anywhere;
}

summary {
  cursor: pointer;
  font-weight: 600;
}

dialog {
  width: min(44rem, calc(100% - 4rem));
  max-height: calc(100% - 3rem);
  padding: 1.25rem;
  border: 1px solid #829d95;
  border-radius: 0.875rem;
  color: inherit;
}

dialog::backdrop {
  background: rgb(18 36 32 / 60%);
}

.detail-header {
  justify-content: space-between;
  align-items: start;
  margin-bottom: 1rem;
}

.detail-header h2 {
  flex: 1;
  overflow-wrap: anywhere;
}

.detail-image {
  display: block;
  width: 100%;
  max-height: 55vh;
  object-fit: contain;
  background: #edf4f1;
}

.detail-path {
  overflow-wrap: anywhere;
}
</style>
