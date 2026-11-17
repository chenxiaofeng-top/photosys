<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import LibraryThumbnail from "./LibraryThumbnail.vue";
import RotatableImage, { type Rotation } from "./RotatableImage.vue";
import { formatBytes } from "./formatBytes";
import { setLibraryThumbnailConcurrency } from "./libraryThumbnailQueue";

type Kind = "photo" | "video";
type Sort = "dateDesc" | "dateAsc" | "nameAsc" | "nameDesc" | "sizeDesc" | "sizeAsc";
type FileAction = "open" | "reveal" | "copy";

interface Item {
  id: number;
  name: string;
  relativePath: string;
  size: number;
  kind: Kind;
  capturedOn: string | null;
  previewAvailable: boolean;
  isLivePhoto: boolean;
  favorite: boolean;
  rotation: Rotation;
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
  thumbnailConcurrency: number;
  preferenceWarning: string | null;
}

interface OpenedLibrary {
  directory: string;
  summary: Summary;
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
const searchInput = ref("");
const query = ref("");
const sort = ref<Sort>("dateDesc");
const composing = ref(false);
let searchTimer: ReturnType<typeof setTimeout> | undefined;
const searchPending = computed(() => composing.value || searchInput.value.trim() !== query.value);

function scheduleSearch() {
  clearTimeout(searchTimer);
  if (!composing.value) searchTimer = setTimeout(() => { query.value = searchInput.value.trim(); }, 250);
}

function startComposition() {
  composing.value = true;
  clearTimeout(searchTimer);
}

function endComposition() {
  composing.value = false;
  scheduleSearch();
}

function clearSearch() {
  clearTimeout(searchTimer);
  searchInput.value = "";
  query.value = "";
}

watch(searchInput, scheduleSearch);
const filter = computed(() => ({
  query: query.value,
  kind: kind.value === "all" ? null : kind.value,
  from: dateMode.value === "range" ? from.value || null : null,
  to: dateMode.value === "range" ? to.value || null : null,
  unknownDate: dateMode.value === "unknown",
  favoritesOnly: favoritesOnly.value,
}));

const requestedPage = ref(0);
const revision = ref(0);
const results = shallowRef<Page<Item>>();
// A page request may have started before a rotation was saved (for example when
// closing a detail on another page). Keep its eventual response up to date.
const savedRotations = new Map<number, Rotation>();
watch(summary, () => savedRotations.clear(), { flush: "sync" });
function withSavedRotations(page: Page<Item>): Page<Item> {
  return {
    ...page,
    items: page.items.map((item) => savedRotations.has(item.id)
      ? { ...item, rotation: savedRotations.get(item.id)! } : item),
  };
}
const itemsLoading = ref(false);
watch([filter, sort], () => { requestedPage.value = 0; }, { flush: "sync" });
watch([summary, filter, sort, requestedPage, revision], async ([scan, currentFilter, currentSort, page], _, onCleanup) => {
  if (!scan) return;
  let receiving = true;
  onCleanup(() => { receiving = false; });
  itemsLoading.value = true;
  itemsError.value = "";
  try {
    const result = await invoke<Page<Item>>("library_items", { scanId: scan.scanId, filter: currentFilter, sort: currentSort, page });
    if (receiving) results.value = withSavedRotations(result);
  } catch (cause) {
    if (receiving) {
      results.value = undefined;
      itemsError.value = String(cause);
    }
  } finally {
    if (receiving) {
      itemsLoading.value = false;
      void restoreCardFocus();
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
const detailImage = ref("");
const previewError = ref("");
const previewReady = ref(false);
const rotationSaving = ref(false);
const rotationError = ref("");
const detailError = ref("");
const fileAction = ref<FileAction>();
const detailStatus = ref("");
const detailPage = shallowRef<Page<Item>>();
const detailPosition = ref(0);
const detailNavigating = ref(false);
const grid = ref<HTMLUListElement>();
let pendingFocus: { scanId: number; fileId: number; page: number } | undefined;
const liveVideo = ref<HTMLVideoElement>();
const liveVideoUrl = ref("");
const liveVideoLoading = ref(false);
const liveVideoError = ref("");
let detailRequest = 0;
const canRotate = computed(() => detail.value?.kind === "photo" && detail.value.previewAvailable &&
  previewReady.value && !previewError.value && !detailNavigating.value && !rotationSaving.value &&
  !liveVideoLoading.value && !liveVideoUrl.value);

async function rotateDetail(delta: -90 | 90) {
  const item = detail.value;
  const scanId = summary.value?.scanId;
  if (!canRotate.value || !item || scanId === undefined || busy.value) return;
  const rotation = ((item.rotation + delta + 360) % 360) as Rotation;
  rotationSaving.value = true;
  task.value = "saving";
  rotationError.value = "";
  try {
    const saved = await invoke<Rotation>("set_library_rotation", { scanId, fileId: item.id, rotation });
    if (summary.value?.scanId !== scanId) return;
    savedRotations.set(item.id, saved);
    const updatePage = (page: Page<Item> | undefined) => page ? {
      ...page,
      items: page.items.map((current) => current.id === item.id ? { ...current, rotation: saved } : current),
    } : undefined;
    results.value = updatePage(results.value);
    detailPage.value = updatePage(detailPage.value);
    if (detail.value?.id === item.id) detail.value = { ...detail.value, rotation: saved };
  } catch (cause) {
    if (summary.value?.scanId === scanId) rotationError.value = String(cause);
  } finally {
    rotationSaving.value = false;
    task.value = undefined;
    void restoreCardFocus();
  }
}

onMounted(async () => {
  try {
    const initial = await invoke<OpenedLibrary | null>("scan_startup_library", { progress: scanProgress() });
    if (initial) showScan(initial.directory, initial.summary);
  } catch (cause) {
    scanError.value = String(cause);
  } finally {
    task.value = undefined;
  }
});

async function chooseDirectory() {
  if (busy.value) return;
  task.value = "choosing";
  try {
    const selected = await invoke<string | null>("choose_folder", { title: "选择媒体库目录" });
    if (selected) await scanDirectory(selected, "select");
  } catch (cause) {
    scanError.value = `无法选择目录。${String(cause)}`;
  } finally {
    task.value = undefined;
  }
}

function scanProgress() {
  return new Channel<{ found: number }>((update) => {
    if (task.value === "starting") task.value = "scanning";
    if (task.value === "scanning") found.value = update.found;
  });
}

function showScan(selected: string, scanned: Summary) {
  setLibraryThumbnailConcurrency(scanned.thumbnailConcurrency);
  if (selected !== directory.value) {
    kind.value = "all";
    dateMode.value = "all";
    from.value = "";
    to.value = "";
    favoritesOnly.value = false;
    clearSearch();
    sort.value = "dateDesc";
  }
  directory.value = selected;
  emit("directoryChange", selected);
  results.value = undefined;
  issues.value = undefined;
  issuesOpen.value = false;
  issuePage.value = 0;
  requestedPage.value = 0;
  summary.value = scanned;
}

async function scanDirectory(selected: string, mode: "select" | "refresh") {
  if (mode === "refresh" && busy.value) return;
  task.value = "scanning";
  scanError.value = "";
  favoriteError.value = "";
  rotationError.value = "";
  found.value = 0;
  try {
    const scanned = await invoke<Summary>("scan_library", { directory: selected, mode, progress: scanProgress() });
    showScan(selected, scanned);
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

watch([summary, filter, sort, revision], () => {
  pendingFocus = undefined;
  dialog.value?.close();
  resetDetail();
}, { flush: "sync" });

function showDetails(item: Item, event: MouseEvent) {
  const page = results.value;
  if (!summary.value || !page || busy.value || itemsLoading.value || searchPending.value) return;
  detailPage.value = page;
  detailPosition.value = page.page * pageSize + page.items.findIndex((current) => current.id === item.id);
  const thumbnail = (event.currentTarget as HTMLButtonElement).querySelector("img");
  void loadDetail(item, thumbnail?.complete && thumbnail.naturalWidth ? thumbnail.currentSrc : "");
}

async function loadDetail(item: Item, thumbnail = "") {
  const scanId = summary.value?.scanId;
  if (scanId === undefined) return;
  const request = ++detailRequest;
  clearLiveVideo();
  detail.value = item;
  rotationError.value = "";
  detailImage.value = thumbnail;
  previewReady.value = false;
  previewError.value = "";
  detailError.value = "";
  detailStatus.value = "";
  fileAction.value = undefined;
  await nextTick();
  if (request !== detailRequest) return;
  if (!dialog.value?.open) dialog.value?.showModal();
  if (!item.previewAvailable) return;
  const isCurrent = () => request === detailRequest && dialog.value?.open;
  try {
    const source = await invoke<string>("library_preview", { scanId, fileId: item.id, detail: true });
    if (!isCurrent()) return;
    const image = new Image();
    image.src = source;
    await image.decode();
    if (isCurrent()) {
      detailImage.value = source;
      previewReady.value = true;
    }
  } catch (cause) {
    if (isCurrent()) {
      detailImage.value = "";
      previewError.value = `无法预览。${String(cause)}`;
    }
  }
}

async function moveDetail(delta: -1 | 1) {
  const scanId = summary.value?.scanId;
  const page = detailPage.value;
  const target = detailPosition.value + delta;
  if (scanId === undefined || !dialog.value?.open || !page || detailNavigating.value || rotationSaving.value || target < 0 || target >= page.total) return;
  const targetPage = Math.floor(target / pageSize);
  let nextPage = page;
  if (targetPage !== page.page) {
    const request = ++detailRequest;
    clearLiveVideo();
    fileAction.value = undefined;
    detailNavigating.value = true;
    detailError.value = "";
    detailStatus.value = "";
    try {
      nextPage = await invoke<Page<Item>>("library_items", {
        scanId, filter: filter.value, sort: sort.value, page: targetPage,
      });
      if (request !== detailRequest || !dialog.value?.open) return;
      nextPage = withSavedRotations(nextPage);
    } catch (cause) {
      if (request === detailRequest && dialog.value?.open) {
        detailNavigating.value = false;
        detailError.value = `无法切换媒体。${String(cause)}`;
        // Restart the current preview if it was still loading when navigation began.
        if (!detailImage.value && detail.value) {
          const error = detailError.value;
          void loadDetail(detail.value);
          detailError.value = error;
        }
      }
      return;
    }
    detailNavigating.value = false;
  }
  const item = nextPage.items[target % pageSize];
  if (!item) return;
  detailPage.value = nextPage;
  detailPosition.value = target;
  void loadDetail(item);
}

function handleDetailKey(event: KeyboardEvent) {
  const target = event.target;
  if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey ||
      (target instanceof Element && target.closest("input, textarea, select, video, [contenteditable]"))) return;
  if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
    event.preventDefault();
    void moveDetail(event.key === "ArrowLeft" ? -1 : 1);
  }
}

function resetDetail() {
  detailRequest++;
  detail.value = undefined;
  detailPage.value = undefined;
  detailImage.value = "";
  previewReady.value = false;
  detailNavigating.value = false;
  fileAction.value = undefined;
  clearLiveVideo();
}

function finishDetails() {
  if (dialog.value?.open || !detail.value) return;
  const scanId = summary.value?.scanId;
  if (scanId !== undefined && detailPage.value) {
    pendingFocus = { scanId, fileId: detail.value.id, page: detailPage.value.page };
    requestedPage.value = detailPage.value.page;
  }
  resetDetail();
  void restoreCardFocus();
}

function closeDetails() {
  dialog.value?.close();
  finishDetails();
}

async function restoreCardFocus() {
  const target = pendingFocus;
  if (!target || detail.value || busy.value || itemsLoading.value || target.scanId !== summary.value?.scanId || results.value?.page !== target.page) return;
  await nextTick();
  if (pendingFocus !== target || detail.value || busy.value || itemsLoading.value || results.value?.page !== target.page) return;
  const card = grid.value?.querySelector<HTMLButtonElement>(`[data-file-id="${target.fileId}"]`);
  if (card) {
    card.focus({ preventScroll: true });
    card.scrollIntoView({ block: "nearest" });
    pendingFocus = undefined;
  }
}

function clearLiveVideo() {
  liveVideo.value?.pause();
  if (liveVideoUrl.value) URL.revokeObjectURL(liveVideoUrl.value);
  liveVideoUrl.value = "";
  liveVideoError.value = "";
  liveVideoLoading.value = false;
}

onBeforeUnmount(() => {
  clearTimeout(searchTimer);
  detailRequest++;
  clearLiveVideo();
});

function failLiveVideo(message: string) {
  clearLiveVideo();
  liveVideoError.value = message;
}

function handleLiveVideoError(event: Event) {
  if (liveVideoUrl.value && event.currentTarget === liveVideo.value) {
    failLiveVideo("当前播放器无法读取实况视频。");
  }
}

async function playLivePhoto() {
  const item = detail.value;
  const scanId = summary.value?.scanId;
  if (!item?.isLivePhoto || scanId === undefined || detailNavigating.value || rotationSaving.value || liveVideoLoading.value || liveVideoUrl.value) return;
  const request = detailRequest;
  const isCurrent = () => request === detailRequest && dialog.value?.open && summary.value?.scanId === scanId;
  liveVideoError.value = "";
  liveVideoLoading.value = true;
  try {
    const bytes = await invoke<ArrayBuffer>("library_live_video", { scanId, fileId: item.id });
    if (!isCurrent()) return;
    liveVideoUrl.value = URL.createObjectURL(new Blob([bytes], { type: "video/webm" }));
    await nextTick();
    if (isCurrent()) await liveVideo.value?.play();
  } catch (cause) {
    if (isCurrent() && !(cause instanceof DOMException && cause.name === "NotAllowedError")) {
      failLiveVideo(`无法播放实况视频。${String(cause)}`);
    }
  } finally {
    if (isCurrent()) liveVideoLoading.value = false;
  }
}

async function performFileAction(action: FileAction) {
  const item = detail.value;
  const scanId = summary.value?.scanId;
  if (!item || scanId === undefined || fileAction.value || detailNavigating.value) return;
  const request = detailRequest;
  fileAction.value = action;
  detailError.value = "";
  detailStatus.value = "";
  const commands: Record<FileAction, string> = {
    open: "open_library_item", reveal: "reveal_library_item", copy: "copy_library_item_path",
  };
  try {
    const result = await invoke<"revealed" | "openedParent" | void>(commands[action], { scanId, fileId: item.id });
    if (request !== detailRequest || !dialog.value?.open) return;
    if (action === "copy") detailStatus.value = "已复制完整路径";
    if (action === "reveal") detailStatus.value = result === "openedParent"
      ? "已打开所在文件夹，无法选中文件。" : "已在文件管理器中显示。";
  } catch (cause) {
    if (request === detailRequest && dialog.value?.open) detailError.value = String(cause);
  } finally {
    if (request === detailRequest) fileAction.value = undefined;
  }
}
</script>

<template>
  <div class="media-library">
    <header class="library-header">
      <h1>媒体库</h1>
      <div v-if="directory" class="directory-toolbar">
        <div class="directory-field">
          <label for="library-directory">媒体库目录</label>
          <input id="library-directory" :value="directory" :title="directory" readonly />
        </div>
        <button type="button" class="secondary" :disabled="busy" @click="chooseDirectory">更换目录</button>
        <button type="button" class="secondary" :disabled="busy" @click="scanDirectory(directory, 'refresh')">刷新</button>
      </div>
    </header>

    <p v-if="task === 'starting'" class="status" role="status">正在恢复媒体库…</p>
    <p v-else-if="task === 'scanning'" class="status" role="status">正在扫描，已发现 {{ found }} 个媒体文件…</p>
    <p v-if="scanError" class="error" role="alert">读取媒体库失败。{{ scanError }}</p>

    <div v-if="!directory && task !== 'starting' && task !== 'scanning'" class="library-empty">
      <h2>打开照片和视频</h2>
      <p>选择包含照片或视频的本地目录。</p>
      <button type="button" :disabled="busy" @click="chooseDirectory">选择目录</button>
    </div>

    <template v-if="summary">
      <div class="filters">
        <label class="search-filter">搜索
          <input v-model="searchInput" type="search" placeholder="文件名或相对路径" aria-label="搜索文件名或相对路径"
            @compositionstart="startComposition" @compositionend="endComposition" />
        </label>
        <button v-if="searchInput" type="button" class="secondary" @click="clearSearch">清空搜索</button>
        <label>排序
          <select v-model="sort">
            <option value="dateDesc">拍摄日期：从新到旧</option>
            <option value="dateAsc">拍摄日期：从旧到新</option>
            <option value="nameAsc">名称：升序</option>
            <option value="nameDesc">名称：降序</option>
            <option value="sizeDesc">文件大小：从大到小</option>
            <option value="sizeAsc">文件大小：从小到大</option>
          </select>
        </label>
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

      <p v-if="summary.preferenceWarning" class="error" role="alert">{{ summary.preferenceWarning }}</p>
      <p v-if="favoriteError" class="error" role="alert">{{ favoriteError }}</p>
      <p v-if="rotationError && !detail" class="error" role="alert">{{ rotationError }}</p>
      <p v-if="itemsError" class="error" role="alert">无法读取媒体列表。{{ itemsError }}</p>
      <p v-else class="result-status status" role="status">
        <template v-if="itemsLoading">正在加载媒体…</template>
        <template v-else-if="results?.total">
          {{ results.total === summary.total ? `共 ${summary.total} 个媒体文件` : `${results.total} / ${summary.total} 个媒体文件` }}
        </template>
      </p>
      <div v-if="!itemsLoading && results?.total === 0" class="library-empty" role="status">
        <h2>{{ summary.total ? "没有符合筛选条件的媒体文件" : "此目录没有可识别的照片或视频" }}</h2>
      </div>

      <ul v-if="results?.items.length" ref="grid" class="media-grid">
        <li v-for="item in results.items" :key="`${summary.scanId}:${item.id}`" class="card">
          <button type="button" class="media-preview secondary" :data-file-id="item.id" :disabled="busy || itemsLoading || searchPending"
            :aria-label="`查看 ${item.name}`" @click="showDetails(item, $event)">
            <LibraryThumbnail v-if="item.previewAvailable" :scan-id="summary.scanId"
              :file-id="item.id" :video="item.kind === 'video'" :rotation="item.rotation" />
            <span v-else>{{ item.kind === "video" ? "▶ 视频" : "照片" }}</span>
          </button>
          <div class="card-details">
            <p class="file-name" :title="item.relativePath">{{ item.name }}</p>
            <div class="card-info">
              <span>{{ item.capturedOn ?? "日期未知" }}</span>
              <button type="button" class="favorite secondary" :aria-label="`收藏 ${item.name}`"
                :title="item.favorite ? '取消收藏' : '收藏'" :aria-pressed="item.favorite"
                :disabled="busy || itemsLoading || searchPending" @click="toggleFavorite(item)">{{ item.favorite ? "★" : "☆" }}</button>
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

    <dialog v-if="detail" ref="dialog" aria-labelledby="library-preview-title"
      @cancel.prevent="closeDetails" @close="finishDetails" @keydown="handleDetailKey">
      <header class="detail-header">
        <h2 id="library-preview-title">{{ detail.name }}</h2>
        <button type="button" class="secondary" @click="closeDetails">关闭</button>
      </header>
      <nav class="detail-navigation" aria-label="连续浏览">
        <button type="button" class="secondary" :disabled="detailNavigating || rotationSaving || detailPosition === 0" @click="moveDetail(-1)">上一项</button>
        <span role="status">第 {{ detailPosition + 1 }} / {{ detailPage?.total ?? 0 }} 项</span>
        <button type="button" class="secondary" :disabled="detailNavigating || rotationSaving || detailPosition + 1 >= (detailPage?.total ?? 0)" @click="moveDetail(1)">下一项</button>
      </nav>
      <p v-if="detailNavigating" class="status" role="status">正在切换媒体…</p>
      <video v-else-if="liveVideoUrl" ref="liveVideo" class="detail-image" :src="liveVideoUrl"
        :aria-label="detail.name" controls playsinline
        @error="handleLiveVideoError" />
      <RotatableImage v-else-if="detailImage" :src="detailImage" :alt="detail.name" :rotation="detail.rotation" detail
        @error="previewReady = false; previewError = '无法显示图片预览。'" />
      <p v-else-if="detail.previewAvailable && !previewError" class="status" role="status">正在加载预览…</p>
      <p v-if="previewError" class="error" role="alert">{{ previewError }}</p>
      <div v-if="detail.kind === 'photo' && detail.previewAvailable && !liveVideoUrl" class="rotation-actions">
        <button type="button" class="secondary" :disabled="!canRotate" @click="rotateDetail(-90)">向左旋转 90°</button>
        <button type="button" class="secondary" :disabled="!canRotate" @click="rotateDetail(90)">向右旋转 90°</button>
        <span class="status" role="status">{{ rotationSaving ? "正在保存方向…" : `已保存方向：${detail.rotation}°` }}</span>
      </div>
      <p v-if="rotationError" class="error" role="alert">{{ rotationError }}</p>
      <p class="detail-path">{{ detail.relativePath }}</p>
      <p class="status">{{ detail.capturedOn ?? "日期未知" }} · {{ formatBytes(detail.size) }}</p>
      <button v-if="detail.isLivePhoto && !liveVideoUrl" type="button" :disabled="detailNavigating || rotationSaving || liveVideoLoading"
        @click="playLivePhoto">
        {{ liveVideoLoading ? "正在加载实况视频…" : "播放实况" }}
      </button>
      <p v-if="liveVideoError" class="error" role="alert">{{ liveVideoError }}</p>
      <div class="detail-actions">
        <button type="button" :disabled="!!fileAction || detailNavigating" @click="performFileAction('open')">
          {{ detail.kind === "video" ? "用系统播放器打开" : "用系统默认应用打开" }}
        </button>
        <button type="button" class="secondary" :disabled="!!fileAction || detailNavigating" @click="performFileAction('reveal')">在文件管理器中显示</button>
        <button type="button" class="secondary" :disabled="!!fileAction || detailNavigating" @click="performFileAction('copy')">复制完整路径</button>
      </div>
      <p v-if="detailStatus" class="status" role="status">{{ detailStatus }}</p>
      <p v-if="detailError" class="error" role="alert">{{ detailError }}</p>
    </dialog>
  </div>
</template>

<style scoped>
.media-library {
  --library-surface: #fdfcf9;
  --library-border: #dedfd8;
  --library-control-border: #a9b5ad;
  --library-muted: #53635d;
  --library-preview: #eaece6;
  color: #24352f;
}

.library-header {
  display: flex;
  flex-wrap: wrap;
  align-items: end;
  justify-content: space-between;
  gap: 0.875rem 2rem;
  margin: 0;
}

.library-header h1 {
  font-size: clamp(1.75rem, 2.2vw, 2.5rem);
  line-height: 1.2;
}

.directory-toolbar {
  display: flex;
  flex: 0 1 44rem;
  flex-wrap: wrap;
  align-items: end;
  gap: 0.5rem;
  min-width: 0;
}

.directory-field {
  flex: 1 1 14rem;
  min-width: 0;
}

.directory-field label {
  margin: 0 0 0.375rem;
  color: var(--library-muted);
  font-size: 0.8125rem;
}

.directory-field input,
.filters :is(select, input[type="date"], input[type="search"]) {
  box-sizing: border-box;
  min-height: 2.5rem;
  padding: 0.375rem 0.625rem;
  border: 1px solid var(--library-control-border);
  border-radius: 0.5rem;
  background: var(--library-surface);
  color: inherit;
  font: inherit;
}

.directory-field input {
  width: 100%;
  font-size: 0.875rem;
}

.directory-field input:not(:focus) {
  text-overflow: ellipsis;
}

.media-library button.secondary:not(.media-preview, .favorite) {
  border-color: var(--library-control-border);
  background: var(--library-surface);
}

.media-library button.secondary:hover:not(.media-preview, .favorite, :disabled) {
  background: #e7efe9;
}

.library-empty {
  display: grid;
  justify-items: center;
  gap: 0.75rem;
  margin-top: 1.25rem;
  padding: clamp(2rem, 6vw, 5rem) 1rem;
  border: 1px solid var(--library-border);
  border-radius: 0.75rem;
  background: var(--library-surface);
  text-align: center;
}

.library-empty p {
  margin: 0;
  color: var(--library-muted);
}

.filters {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.625rem 1.25rem;
  margin-top: 1.25rem;
  padding-block: 0.75rem;
  border-block: 1px solid var(--library-border);
}

.filters label {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  margin: 0;
  font-size: 0.875rem;
}

.filters :is(select, input):focus-visible {
  outline: 2px solid #126353;
  outline-offset: 2px;
}

.favorite-filter {
  min-height: 2.5rem;
}

.favorite-filter input {
  width: 1rem;
  height: 1rem;
  margin: 0;
  accent-color: #176252;
}

.status {
  color: var(--library-muted);
}

.result-status {
  margin: 1rem 0 0.75rem;
  font-size: 0.875rem;
}

.result-status:empty {
  margin: 0;
}

.media-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: clamp(0.625rem, 1vw, 1rem);
  margin: 0;
  padding: 0;
  list-style: none;
}

.card {
  min-width: 0;
  border: 1px solid var(--library-border);
  border-radius: 0.75rem;
  background: var(--library-surface);
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
  background: var(--library-preview);
  color: var(--library-muted);
  font-size: 0.875rem;
}

.card-details {
  padding: 0.625rem 0.75rem 0.25rem;
}

.file-name {
  margin: 0;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
  font-size: 0.875rem;
  font-weight: 650;
}

.card-info {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 0.25rem;
  color: var(--library-muted);
  font-size: 0.8125rem;
  font-variant-numeric: tabular-nums;
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

button.favorite:hover:not(:disabled) {
  background: #e7efe9;
}

.pagination,
.detail-navigation,
.rotation-actions,
.detail-actions,
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
  background: var(--library-surface);
  color: inherit;
}

dialog::backdrop {
  background: rgb(18 36 32 / 60%);
}

.detail-navigation {
  justify-content: center;
  margin-bottom: 1rem;
}

.detail-navigation span { font-variant-numeric: tabular-nums; }
.detail-actions { margin-top: 1rem; }
.rotation-actions { margin-top: 0.75rem; }
.search-filter input { min-width: 0; width: 14rem; max-width: 100%; }

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
  background: var(--library-preview);
}

.detail-path {
  overflow-wrap: anywhere;
}

@media (min-width: 40rem) {
  .media-grid { grid-template-columns: repeat(3, minmax(0, 1fr)); }
}

@media (min-width: 48rem) {
  .media-grid { grid-template-columns: repeat(4, minmax(0, 1fr)); }
}

@media (min-width: 72rem) {
  .media-grid { grid-template-columns: repeat(5, minmax(0, 1fr)); }
}

@media (min-width: 90rem) {
  .media-grid { grid-template-columns: repeat(6, minmax(0, 1fr)); }
}
</style>
