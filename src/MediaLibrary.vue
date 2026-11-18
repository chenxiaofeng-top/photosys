<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import { ElButton, ElCheckbox, ElIcon, ElInput, ElOption, ElSelect } from "element-plus";
import { FolderOpened, Refresh, Search, Star, StarFilled, Picture, Warning, Loading } from "@element-plus/icons-vue";
import "element-plus/es/components/button/style/css";
import "element-plus/es/components/checkbox/style/css";
import "element-plus/es/components/icon/style/css";
import "element-plus/es/components/input/style/css";
import "element-plus/es/components/option/style/css";
import "element-plus/es/components/select/style/css";
import LibraryThumbnail from "./LibraryThumbnail.vue";
import RotatableImage, { type Rotation } from "./RotatableImage.vue";
import AiSearchSettings from "./AiSearchSettings.vue";
import PeoplePanel from "./PeoplePanel.vue";
import PeopleFaceEditor from "./PeopleFaceEditor.vue";
import type { Person, PeopleStatus, Month } from "./people";
import type { IndexStatus } from "./aiSearch";
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

const props = defineProps<{ peopleMode?: boolean }>();
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
const searchMode = ref<"filename" | "content">("filename");
const aiReady = ref(false);
const aiIndex = ref<IndexStatus>();
const aiPanel = ref<InstanceType<typeof AiSearchSettings>>();
const aiRevision = ref(0);
const sort = ref<Sort>("dateDesc");
const selectedPerson = shallowRef<Person>();
const peopleVisited = ref(false);
const peopleRevision = ref(0);
const peopleStatus = shallowRef<PeopleStatus>();
const months = shallowRef<Month[]>([]);
const timelineError = ref("");
function personChanged() { peopleRevision.value++; revision.value++; }
function selectPerson(person: Person | undefined) {
  selectedPerson.value = person;
  requestedPage.value = 0;
  if (person) { dateMode.value = "all"; from.value = ""; to.value = ""; favoritesOnly.value = false; sort.value = "dateAsc"; }
}
function selectMonth(month: string | null) {
  if (month === null) { dateMode.value = "unknown"; return; }
  from.value = `${month}-01`;
  const [year, number] = month.split("-").map(Number);
  const end = new Date(Date.UTC(year!, number!, 0)).getUTCDate();
  to.value = `${month}-${String(end).padStart(2, "0")}`;
  dateMode.value = "range";
}
function monthTitle(date: string | null) { return date ? `${date.slice(0, 4)} 年 ${Number(date.slice(5, 7))} 月` : "日期未知"; }
let libraryPreferences: { searchMode: typeof searchMode.value; sort: Sort; kind: typeof kind.value; dateMode: typeof dateMode.value; from: string; to: string; favoritesOnly: boolean; input: string; query: string } | undefined;
watch(() => props.peopleMode, (people) => {
  if (people) {
    peopleVisited.value = true;
    libraryPreferences = { searchMode: searchMode.value, sort: sort.value, kind: kind.value, dateMode: dateMode.value, from: from.value, to: to.value, favoritesOnly: favoritesOnly.value, input: searchInput.value, query: query.value };
    searchMode.value = "filename"; clearSearch(); kind.value = "photo"; dateMode.value = "all"; favoritesOnly.value = false; sort.value = "dateAsc";
  } else if (libraryPreferences) {
    const p = libraryPreferences; searchMode.value = p.searchMode; sort.value = p.sort; kind.value = p.kind; dateMode.value = p.dateMode; from.value = p.from; to.value = p.to; favoritesOnly.value = p.favoritesOnly;
    nextTick(() => { searchInput.value = p.input; query.value = p.query; });
    libraryPreferences = undefined;
  }
}, { immediate: true });
const composing = ref(false);
let searchTimer: ReturnType<typeof setTimeout> | undefined;
const searchPending = computed(() => searchMode.value === "filename" && (composing.value || searchInput.value.trim() !== query.value));

function scheduleSearch() {
  clearTimeout(searchTimer);
  if (!composing.value && searchMode.value === "filename") searchTimer = setTimeout(() => { query.value = searchInput.value.trim(); }, 250);
}

function submitContentSearch(event?: KeyboardEvent) {
  if (composing.value || event?.isComposing || !aiReady.value) return;
  query.value = searchInput.value.trim();
  requestedPage.value = 0;
  aiRevision.value++;
}

watch(searchMode, () => { clearSearch(); requestedPage.value = 0; });
watch(aiReady, () => { if (searchMode.value === "content") aiRevision.value++; });

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
async function fetchPage(scanId: number, currentFilter: typeof filter.value, currentSort: Sort, page: number): Promise<Page<Item>> {
  if (props.peopleMode) {
    if (!selectedPerson.value) return { page: 0, total: 0, items: [] };
    return invoke<Page<Item>>("people_photos", { scanId, personId: selectedPerson.value.id, filter: currentFilter, sort: currentSort, page });
  }
  if (searchMode.value === "content") {
    if (!currentFilter.query) return { page: 0, total: 0, items: [] };
    return invoke<Page<Item>>("ai_search", { scanId, query: currentFilter.query, filter: currentFilter, page });
  }
  return invoke<Page<Item>>("library_items", { scanId, filter: currentFilter, sort: currentSort, page });
}
watch([filter, sort], () => { requestedPage.value = 0; }, { flush: "sync" });
watch([summary, filter, sort, requestedPage, revision, searchMode, aiRevision, () => props.peopleMode, selectedPerson], async ([scan, currentFilter, currentSort, page], _, onCleanup) => {
  if (!scan) return;
  let receiving = true;
  onCleanup(() => { receiving = false; });
  itemsLoading.value = true;
  itemsError.value = "";
  try {
    const result = await fetchPage(scan.scanId, currentFilter, currentSort, page);
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

watch([summary, selectedPerson, favoritesOnly, sort, peopleRevision, () => props.peopleMode], async (_, __, cleanup) => {
  let current = true; cleanup(() => { current = false; });
  const scanId = summary.value?.scanId; const personId = selectedPerson.value?.id;
  months.value = []; timelineError.value = "";
  if (!props.peopleMode || scanId === undefined || personId === undefined) return;
  try {
    const value = await invoke<Month[]>("people_months", { scanId, personId, filter: { query: "", kind: "photo", from: null, to: null, unknownDate: false, favoritesOnly: favoritesOnly.value }, sort: sort.value });
    if (current) months.value = value;
  } catch (cause) { if (current) timelineError.value = String(cause); }
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
const detailPeople = ref<string[]>([]);
let peopleResultsDirty = false;
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
    void aiPanel.value?.refreshIndex();
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
    selectedPerson.value = undefined;
    peopleStatus.value = undefined;
    libraryPreferences = undefined;
    kind.value = "all";
    dateMode.value = "all";
    from.value = "";
    to.value = "";
    favoritesOnly.value = false;
    clearSearch();
    sort.value = "dateDesc";
  }
  directory.value = selected;
  aiIndex.value = undefined;
  emit("directoryChange", selected);
  results.value = undefined;
  issues.value = undefined;
  issuesOpen.value = false;
  issuePage.value = 0;
  requestedPage.value = 0;
  summary.value = scanned;
  peopleRevision.value++;
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

watch([summary, filter, sort, revision, searchMode, selectedPerson, () => props.peopleMode], () => {
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

async function showPersonPhoto(fileId: number) {
  const scanId = summary.value?.scanId;
  if (scanId === undefined || busy.value) return;
  try {
    const item = await invoke<Item>("library_item", { scanId, fileId });
    if (summary.value?.scanId !== scanId) return;
    detailPage.value = { page: 0, total: 1, items: [item] }; detailPosition.value = 0;
    await loadDetail(item);
  } catch (cause) { favoriteError.value = String(cause); }
}
function detailPeopleChanged() { peopleRevision.value++; peopleResultsDirty = true; }
watch([detail, peopleRevision], async (_, __, cleanup) => {
  let current = true; cleanup(() => { current = false; }); detailPeople.value = [];
  const scanId = summary.value?.scanId; const item = detail.value;
  if (scanId === undefined || !item || item.kind !== "photo") return;
  try {
    const value = await invoke<{ id: number; name: string }[]>("people_photo_people", { scanId, fileId: item.id });
    if (current) detailPeople.value = value.map(p => p.name || `未命名人物 ${p.id}`);
  } catch (cause) { if (current) detailError.value = `无法读取人物标注。${String(cause)}`; }
});

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
      nextPage = await fetchPage(scanId, filter.value, sort.value, targetPage);
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
  if (peopleResultsDirty) { peopleResultsDirty = false; revision.value++; }
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
      <div class="library-heading">
        <h1>{{ peopleMode ? "人物与家庭时间线" : "媒体库" }}</h1>
        <p>把每一段回忆，好好珍藏。</p>
      </div>
      <div v-if="directory" class="directory-toolbar">
        <div class="directory-field">
          <label for="library-directory">当前目录</label>
          <ElInput id="library-directory" :model-value="directory" :title="directory" readonly :prefix-icon="FolderOpened" />
        </div>
        <ElButton :icon="FolderOpened" :disabled="busy" @click="chooseDirectory">更换目录</ElButton>
        <ElButton :icon="Refresh" :loading="task === 'scanning'" :disabled="busy" @click="scanDirectory(directory, 'refresh')">刷新</ElButton>
      </div>
    </header>

    <AiSearchSettings v-show="!peopleMode" ref="aiPanel" :scan-id="summary?.scanId" :library-busy="busy"
      @model-ready="aiReady = $event" @index-change="aiIndex = $event" @updated="aiRevision++" />

    <div v-if="task === 'starting' || task === 'scanning'" class="scan-status" role="status">
      <ElIcon class="is-loading" aria-hidden="true"><Loading /></ElIcon>
      {{ task === 'starting' ? '正在恢复媒体库…' : `正在扫描，已发现 ${found} 个媒体文件…` }}
    </div>
    <div v-if="scanError" class="library-notice" role="alert">
      <ElIcon aria-hidden="true"><Warning /></ElIcon>
      <span>读取媒体库失败。{{ scanError }}</span>
      <ElButton v-if="directory" :disabled="busy" @click="scanDirectory(directory, 'refresh')">重试</ElButton>
    </div>

    <div v-if="!directory && task !== 'starting' && task !== 'scanning'" class="library-empty">
      <ElIcon class="empty-icon" aria-hidden="true"><FolderOpened /></ElIcon>
      <h2>打开你的照片与视频</h2>
      <p>选择本地相册目录，开始浏览与收藏美好瞬间。</p>
      <ElButton type="primary" :icon="FolderOpened" :disabled="busy" @click="chooseDirectory">选择目录</ElButton>
      <span class="empty-hint">照片保留在原目录，安心浏览</span>
    </div>

    <PeoplePanel v-if="summary && peopleVisited" v-show="peopleMode" :scan-id="summary.scanId" :library-busy="busy"
      :selected="selectedPerson" :revision="peopleRevision" @select="selectPerson" @changed="personChanged" @preview="showPersonPhoto" @status="peopleStatus = $event" />

    <template v-if="summary && (!peopleMode || selectedPerson)">
      <p v-if="peopleMode" class="timeline-intro">按拍摄日期浏览成长记录，未知日期单独列出。</p>
      <nav v-if="peopleMode && months?.length" class="timeline-months" aria-label="成长时间线年月导航">
        <button type="button" class="secondary" :aria-pressed="dateMode === 'all'" @click="dateMode = 'all'">全部日期</button>
        <button v-for="entry in months" :key="entry.month ?? 'unknown'" type="button" class="secondary"
          :aria-pressed="entry.month ? dateMode === 'range' && from === `${entry.month}-01` : dateMode === 'unknown'" @click="selectMonth(entry.month)">{{ monthTitle(entry.month) }} · {{ entry.count }}</button>
      </nav>
      <p v-if="timelineError" class="error" role="alert">{{ timelineError }}</p>
      <div class="library-tools" :class="{ 'people-tools': peopleMode }" role="search" aria-label="媒体搜索与筛选">
        <div v-if="!peopleMode" class="search-row">
          <ElSelect v-model="searchMode" class="mode-select" aria-label="搜索方式">
            <ElOption label="文件名" value="filename" />
            <ElOption label="照片内容" value="content" />
          </ElSelect>
          <ElInput v-model="searchInput" class="search-input" type="search" clearable :prefix-icon="Search"
            :placeholder="searchMode === 'content' ? '描述照片，例如：海边日落' : '搜索文件名或相对路径…'"
            :aria-label="searchMode === 'content' ? '描述照片内容' : '搜索文件名或相对路径'"
            @clear="clearSearch" @keydown.enter="searchMode === 'content' && submitContentSearch($event as KeyboardEvent)"
            @compositionstart="startComposition" @compositionend="endComposition" />
          <ElButton v-if="searchMode === 'content'" type="primary" :icon="Search"
            :disabled="busy || !aiReady || !searchInput.trim() || composing" @click="submitContentSearch()">搜索照片</ElButton>
        </div>
        <div class="filters">
          <div v-if="!peopleMode && searchMode === 'filename'" class="filter-field">
            <span id="kind-label">类型</span>
            <ElSelect v-model="kind" class="kind-select" aria-label="媒体类型">
              <ElOption label="全部类型" value="all" /><ElOption label="照片" value="photo" /><ElOption label="视频" value="video" />
            </ElSelect>
          </div>
          <div class="filter-field">
            <span id="date-label">拍摄日期</span>
            <ElSelect v-model="dateMode" class="date-select" aria-label="拍摄日期">
              <ElOption label="全部日期" value="all" /><ElOption label="日期范围" value="range" /><ElOption label="日期未知" value="unknown" />
            </ElSelect>
          </div>
          <div v-if="dateMode === 'range'" class="date-range">
            <label>起始日期<input v-model="from" type="date" /></label>
            <label>结束日期<input v-model="to" type="date" /></label>
          </div>
          <ElCheckbox v-model="favoritesOnly" class="favorite-filter">仅收藏</ElCheckbox>
          <div v-if="searchMode === 'filename'" class="filter-field sort-field">
            <span id="sort-label">排序</span>
            <ElSelect v-model="sort" class="sort-select" aria-label="排序">
              <ElOption label="拍摄日期：从新到旧" value="dateDesc" /><ElOption label="拍摄日期：从旧到新" value="dateAsc" />
              <ElOption v-if="!peopleMode" label="名称：升序" value="nameAsc" /><ElOption v-if="!peopleMode" label="名称：降序" value="nameDesc" />
              <ElOption v-if="!peopleMode" label="文件大小：从大到小" value="sizeDesc" /><ElOption v-if="!peopleMode" label="文件大小：从小到大" value="sizeAsc" />
            </ElSelect>
          </div>
          <span v-else class="relevance-note">按内容相关度排序 · 最多 200 张</span>
        </div>
      </div>

      <p v-if="searchMode === 'content' && !aiReady" role="status">请在“AI 搜索设置”中选择模型目录并下载安装。</p>
      <p v-else-if="searchMode === 'content' && !aiIndex?.enabled" role="status">请先点击“建立索引”，再描述照片内容进行搜索。</p>
      <p v-if="searchMode === 'content' && aiIndex?.enabled && (aiIndex.pending || aiIndex.running || aiIndex.failed)" role="status">
        当前搜索覆盖 {{ aiIndex.indexed }} 张照片，{{ aiIndex.pending }} 张待处理<span v-if="aiIndex.failed">，{{ aiIndex.failed }} 张处理失败</span>。
      </p>

      <p v-if="summary.preferenceWarning" class="error" role="alert">{{ summary.preferenceWarning }}</p>
      <p v-if="favoriteError" class="error" role="alert">{{ favoriteError }}</p>
      <p v-if="rotationError && !detail" class="error" role="alert">{{ rotationError }}</p>
      <div v-if="itemsError" class="library-notice" role="alert">
        <ElIcon aria-hidden="true"><Warning /></ElIcon><span>无法读取媒体列表。{{ itemsError }}</span>
        <ElButton :disabled="busy || itemsLoading" @click="revision++">重试</ElButton>
      </div>
      <p v-else class="result-status status" role="status">
        <template v-if="itemsLoading">正在加载媒体…</template>
        <template v-else-if="results?.total">
          <template v-if="peopleMode">共 {{ results.total }} 张人物照片</template>
          <template v-else>{{ results.total === summary.total ? `共 ${summary.total} 个媒体文件` : `${results.total} / ${summary.total} 个媒体文件` }}</template>
        </template>
      </p>
      <div v-if="itemsLoading && !results?.items.length" class="library-empty" role="status">
        <ElIcon class="empty-icon is-loading" aria-hidden="true"><Loading /></ElIcon>
        <h2>正在加载媒体…</h2>
        <p>精彩回忆，即将呈现。</p>
      </div>
      <div v-else-if="!itemsLoading && !itemsError && results?.total === 0" class="library-empty" role="status">
        <ElIcon class="empty-icon" aria-hidden="true"><component :is="searchMode === 'content' && !query ? Search : Picture" /></ElIcon>
        <h2>{{ searchMode === 'content' && !query ? '用一句话，找回那个瞬间' : summary.total ? '没有找到匹配的照片或视频' : '这里还没有照片或视频' }}</h2>
        <p>{{ searchMode === 'content' && !query ? '输入照片描述，按回车或点击“搜索照片”。' : summary.total ? '试试其他关键词，或调整类型、日期和收藏筛选。' : '在目录中添加媒体文件后，点击刷新。' }}</p>
        <ElButton v-if="summary.total && (query || kind !== 'all' || dateMode !== 'all' || favoritesOnly)"
          @click="clearSearch(); kind = 'all'; dateMode = 'all'; favoritesOnly = false">清除搜索与筛选</ElButton>
        <ElButton v-else-if="!summary.total" :icon="Refresh" :disabled="busy" @click="scanDirectory(directory, 'refresh')">刷新目录</ElButton>
      </div>

      <ul v-if="results?.items.length" ref="grid" class="media-grid" :aria-busy="itemsLoading">
        <template v-for="(item, index) in results.items" :key="`${summary.scanId}:${item.id}`">
        <li v-if="peopleMode && (index === 0 || item.capturedOn?.slice(0, 7) !== results.items[index - 1]?.capturedOn?.slice(0, 7))" class="timeline-heading">{{ monthTitle(item.capturedOn) }}</li>
        <li class="card">
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
              <button type="button" class="favorite secondary" :aria-label="`${item.favorite ? '取消收藏' : '收藏'} ${item.name}`"
                :title="item.favorite ? '取消收藏' : '收藏'" :aria-pressed="item.favorite"
                :disabled="busy || itemsLoading || searchPending" @click="toggleFavorite(item)"><ElIcon aria-hidden="true"><component :is="item.favorite ? StarFilled : Star" /></ElIcon></button>
            </div>
          </div>
        </li>
        </template>
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
      <p v-if="detailPeople.length" class="status">照片中的人物：{{ detailPeople.join("、") }}</p>
      <details v-if="summary && detail.kind === 'photo'" class="detail-people">
        <summary>照片中的人物 / 整理人脸</summary>
        <PeopleFaceEditor :scan-id="summary.scanId" :file-id="detail.id" :revision="peopleRevision" :writable="peopleStatus?.writable" :busy="busy || rotationSaving"
          @changed="detailPeopleChanged" />
      </details>
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
.timeline-months { display: flex; flex-wrap: wrap; gap: .5rem; max-height: 12rem; overflow-y: auto; margin: 1rem 0; }
.timeline-months button { padding: .45rem .65rem; font-size: .85rem; }
.timeline-months [aria-pressed="true"] { color: #fff; background: #176252; border-color: #176252; }
.timeline-heading { grid-column: 1 / -1; padding-top: .75rem; list-style: none; color: #3f6252; font-size: 1.25rem; font-weight: 650; }
.timeline-intro { color: #65736b; }
.people-tools .filters { margin-top: 0; padding-top: 0; border-top: 0; }
@media (max-width: 34rem) {
  .people-tools .sort-field { flex: 1 1 100%; }
  .people-tools .sort-select { flex: 1; min-width: 0; width: auto; }
}
.detail-people { margin-top: 1rem; padding: .75rem; border: 1px solid #dce5dc; border-radius: .5rem; }

.media-library {
  --library-surface: #fff;
  --library-border: #e0e5df;
  --library-control-border: #cbd6ce;
  --library-muted: #65736b;
  --library-preview: #eef0ea;
  color: #24352f;
}
.library-header { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 1.5rem 2rem; margin: 0 0 1.5rem; }
.library-heading h1 { font-size: clamp(1.8rem, 2.2vw, 2.5rem); line-height: 1.3; letter-spacing: -0.03em; }
.library-heading p { margin: 0.4rem 0 0; color: var(--library-muted); }
.directory-toolbar { display: flex; flex: 0 1 43rem; align-items: end; gap: 0.625rem; min-width: 0; max-width: 100%; }
.directory-field { flex: 1; min-width: 0; }
.directory-field label { margin: 0 0 0.375rem; color: var(--library-muted); font-size: 0.8125rem; font-weight: 500; }
.directory-field :deep(input:not(:focus)) { text-overflow: ellipsis; }
.library-tools { padding: 1.25rem; border: 1px solid var(--library-border); border-radius: 1rem; background: #fff; }
.search-row { display: flex; gap: 0.75rem; min-width: 0; }
.mode-select { flex: 0 0 8.5rem; }
.search-input { flex: 1; min-width: 0; }
.filters { display: flex; flex-wrap: wrap; align-items: center; gap: 0.75rem 1.25rem; margin-top: 1rem; padding-top: 1rem; border-top: 1px solid #edf0eb; }
.filter-field { display: flex; align-items: center; gap: 0.625rem; min-width: 0; }
.filter-field > span { color: var(--library-muted); white-space: nowrap; font-size: 0.875rem; }
.kind-select { width: 8rem; }
.date-select { width: 8rem; }
.sort-select { width: 13rem; }
.sort-field { margin-left: auto; }
.favorite-filter { margin-right: 0; }
.date-range { display: flex; flex-wrap: wrap; gap: 0.75rem; }
.date-range label { display: flex; align-items: center; gap: 0.5rem; margin: 0; font-size: 0.875rem; font-weight: 400; color: var(--library-muted); }
.date-range input { box-sizing: border-box; min-height: 40px; max-width: 100%; padding: 0.375rem 0.625rem; border: 1px solid var(--library-control-border); border-radius: 8px; color: #24352f; background: #fff; font: inherit; }
.relevance-note { margin-left: auto; color: var(--library-muted); font-size: 0.875rem; }
.scan-status, .library-notice { display: flex; flex-wrap: wrap; align-items: center; gap: 0.625rem; margin: 1rem 0; padding: 0.875rem 1rem; border-radius: 0.75rem; background: #edf4f0; color: #176252; }
.library-notice { background: #fff2ec; color: #9c3726; }
.library-notice span { flex: 1; min-width: 12rem; overflow-wrap: anywhere; }
.library-empty { display: grid; justify-items: center; gap: 0.875rem; margin-top: 1.5rem; padding: clamp(2.5rem, 6vw, 5rem) 1rem; border: 1px dashed #d8e0d7; border-radius: 1rem; background: #ffffffa6; text-align: center; }
.library-empty p { margin: 0; color: var(--library-muted); }
.empty-icon { box-sizing: border-box; width: 64px; height: 64px; margin-bottom: 0.5rem; border-radius: 20px; background: #e7f0e9; color: #176252; font-size: 30px; }
.empty-hint { font-size: 0.8125rem; color: var(--library-muted); }
@media (max-width: 60rem) {
  .library-header { align-items: end; gap: 1rem; }
  .library-heading p { display: none; }
  .directory-toolbar { flex: 1 1 26rem; }
  .filters { column-gap: 0.75rem; }
  .filter-field { gap: 0.5rem; }
  .kind-select, .date-select { width: 7.5rem; }
  .sort-select { width: 13rem; }
}
@media (max-width: 48rem) {
  .directory-toolbar { flex-basis: 100%; }
  .sort-field { margin-left: 0; }
}
@media (max-width: 34rem) {
  .directory-toolbar { flex-wrap: wrap; }
  .directory-field { flex-basis: 100%; }
  .search-row { flex-wrap: wrap; }
  .search-input { flex-basis: calc(100% - 10rem); }
  .search-row > .el-button { width: 100%; }
  .sort-field, .sort-select { flex: 1; }
  .relevance-note { margin-left: 0; }
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
  gap: clamp(1rem, 1.4vw, 1.5rem);
  margin: 0;
  padding: 0;
  list-style: none;
}

.card {
  min-width: 0;
  border: 1px solid #e7eae4;
  border-radius: 0.875rem;
  background: var(--library-surface);
  transition: border-color 160ms ease, box-shadow 160ms ease;
}

.card:hover, .card:focus-within { border-color: #b6cbbd; box-shadow: 0 5px 18px #24352f0a; }

button.media-preview {
  display: grid;
  width: 100%;
  aspect-ratio: 4 / 3;
  grid-template: minmax(0, 1fr) / minmax(0, 1fr);
  place-items: center;
  padding: 0;
  overflow: hidden;
  border: 0;
  border-radius: 0.8rem 0.8rem 0 0;
  background: var(--library-preview);
  color: var(--library-muted);
  font-size: 0.875rem;
}

.card-details {
  padding: 0.875rem 0.875rem 0.375rem;
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
  display: grid;
  place-items: center;
  min-width: 2.75rem;
  min-height: 2.75rem;
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
  justify-content: center;
  margin-top: 1.75rem;
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
