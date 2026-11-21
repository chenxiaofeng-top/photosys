<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, defineAsyncComponent, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from "vue";
import { ElButton, ElCheckbox, ElIcon, ElInput, ElOption, ElSelect } from "element-plus";
import { FolderOpened, Refresh, Search, Star, StarFilled, Picture, Warning, Loading, VideoPlay } from "@element-plus/icons-vue";
import "element-plus/es/components/button/style/css";
import "element-plus/es/components/checkbox/style/css";
import "element-plus/es/components/icon/style/css";
import "element-plus/es/components/input/style/css";
import "element-plus/es/components/option/style/css";
import "element-plus/es/components/select/style/css";
import LibraryThumbnail from "./LibraryThumbnail.vue";
import MediaDetailsInfo from "./MediaDetailsInfo.vue";
import RotatableImage, { type Rotation } from "./RotatableImage.vue";
import AiSearchSettings from "./AiSearchSettings.vue";
import DatabaseBackup from "./DatabaseBackup.vue";
import DateFilterPicker from "./DateFilterPicker.vue";
import { ALL_DATES, UNKNOWN_DATES, monthDateRange, sameDateFilter, type DateFilter } from "./dateFilter";
import type { DatabaseRestoreResult, LibrarySummary } from "./databaseBackup";
import PeoplePanel from "./PeoplePanel.vue";
import AlbumsPanel from "./AlbumsPanel.vue";
import AlbumPicker from "./AlbumPicker.vue";
import type { Album, AlbumEditResult } from "./albums";
import type { Item, Page, Kind, Sort, LibraryMode, MediaDetails } from "./libraryTypes";
import { useAlbumSelection } from "./albumSelection";
import PeopleFaceEditor from "./PeopleFaceEditor.vue";
const LocationMap = defineAsyncComponent(() => import("./LocationMap.vue"));
import { shortLocation, type Coordinates, type LocationMode, type Selection } from "./locations";
import type { Person, PeopleStatus, Month } from "./people";
import type { IndexStatus } from "./aiSearch";
import { setLibraryThumbnailConcurrency } from "./libraryThumbnailQueue";

type FileAction = "open" | "reveal" | "copy";
type LibraryView = "grid" | "timeline";

interface Issue {
  path: string;
  reason: string;
}

type Summary = LibrarySummary;

interface OpenedLibrary {
  directory: string;
  summary: Summary;
}

const props = withDefaults(defineProps<{ mode?: LibraryMode; active?: boolean }>(), { mode: "library", active: true });
const peopleMode = computed(() => props.mode === "people");
const albumsMode = computed(() => props.mode === "albums");
const emit = defineEmits<{ busyChange: [busy: boolean]; directoryChange: [directory: string]; modeChange: [mode: LibraryMode] }>();
const pageSize = 24;
const directory = ref("");
const task = ref<"starting" | "choosing" | "scanning" | "saving" | "database" | undefined>("starting");
const busy = computed(() => task.value !== undefined);
watch(busy, (value) => emit("busyChange", value), { flush: "sync", immediate: true });

const summary = shallowRef<Summary>();
const found = ref(0);
const scanError = ref("");
const favoriteError = ref("");
const itemsError = ref("");
const issuesError = ref("");

const kind = ref<Kind | "all">("all");
const dateFilter = shallowRef<DateFilter>(ALL_DATES);
function setDateFilter(next: DateFilter) {
  if (!sameDateFilter(dateFilter.value, next)) dateFilter.value = next;
}
const favoritesOnly = ref(false);
const locationMode = ref<LocationMode>("all");
const locationSelection = shallowRef<Selection | null>(null);
const mapFocus = shallowRef<Coordinates | null>(null);
const locationLabel = computed(() => locationSelection.value?.type === "point" ? "已选择拍摄点位" : locationSelection.value ? "已选择地图范围" : "");
function selectLocation(selection: Selection | null) {
  locationSelection.value = selection;
  if (selection) locationMode.value = "known";
}
watch(locationMode, mode => { if (mode !== "known") locationSelection.value = null; });
async function locateDetail() {
  const location = detail.value?.location;
  if (!location) return;
  closeDetails(); await nextTick();
  mapFocus.value = { latitude: location.latitude, longitude: location.longitude };
}
const searchInput = ref("");
const query = ref("");
const searchMode = ref<"filename" | "content">("filename");
const aiReady = ref(false);
const aiIndex = ref<IndexStatus>();
const aiPanel = ref<InstanceType<typeof AiSearchSettings>>();
const aiRevision = ref(0);
const sort = ref<Sort>("dateDesc");
const timelineSort = ref<"dateDesc" | "dateAsc">("dateDesc");
const libraryViewKey = "photosys.libraryView";
const libraryView = ref<LibraryView>("grid");
try {
  if (window.localStorage.getItem(libraryViewKey) === "timeline") libraryView.value = "timeline";
} catch { /* The current session can still change views. */ }

const canChooseLibraryView = computed(() => props.mode === "library" && searchMode.value === "filename");
const libraryTimeline = computed(() => canChooseLibraryView.value && libraryView.value === "timeline");
const activeSort = computed<Sort>({
  get: () => libraryTimeline.value ? timelineSort.value : sort.value,
  set: (value) => {
    if (!libraryTimeline.value) sort.value = value;
    else if (value === "dateDesc" || value === "dateAsc") timelineSort.value = value;
  },
});

function setLibraryView(value: LibraryView) {
  libraryView.value = value;
  try { window.localStorage.setItem(libraryViewKey, value); }
  catch { /* The current session can still change views. */ }
}

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
  if (person) { dateFilter.value = ALL_DATES; favoritesOnly.value = false; sort.value = "dateAsc"; }
}
function selectMonth(month: string | null) {
  setDateFilter(month === null ? UNKNOWN_DATES : monthDateRange(month));
}
function monthSelected(month: string | null) {
  return sameDateFilter(dateFilter.value, month === null ? UNKNOWN_DATES : monthDateRange(month));
}
function monthTitle(date: string | null) { return date ? `${date.slice(0, 4)} 年 ${Number(date.slice(5, 7))} 月` : "日期未知"; }
let libraryPreferences: { searchMode: typeof searchMode.value; sort: Sort; kind: typeof kind.value; dateFilter: DateFilter; favoritesOnly: boolean; input: string; query: string } | undefined;
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
  from: dateFilter.value.mode === "range" ? dateFilter.value.from : null,
  to: dateFilter.value.mode === "range" ? dateFilter.value.to : null,
  unknownDate: dateFilter.value.mode === "unknown",
  favoritesOnly: favoritesOnly.value,
  locationMode: albumsMode.value ? "all" as const : locationMode.value,
  locationSelection: albumsMode.value ? null : locationSelection.value,
}));

const selectedAlbum = shallowRef<Album>();
const albumsRevision = ref(0);
const albumsWritable = ref(false);
const targetAlbum = shallowRef<Album>();
const addedAlbum = shallowRef<Album>();
const albumError = ref("");
const albumPicker = ref<InstanceType<typeof AlbumPicker>>();
const { selected: selectedFiles, selecting, clear: clearSelection, toggle: toggleSelection, selectPage, stop: stopSelection } = useAlbumSelection([
  summary, filter, activeSort, searchMode, () => props.mode, () => selectedAlbum.value?.id,
]);
const requestedPage = ref(0);
const revision = ref(0);
watch(() => props.mode, (mode, oldMode) => {
  const people = mode === "people";
  if (mode !== "library") {
    if (people) peopleVisited.value = true;
    if (oldMode === "library" || oldMode === undefined) libraryPreferences = { searchMode: searchMode.value, sort: sort.value, kind: kind.value, dateFilter: dateFilter.value, favoritesOnly: favoritesOnly.value, input: searchInput.value, query: query.value };
    searchMode.value = "filename"; clearSearch(); kind.value = people ? "photo" : "all"; dateFilter.value = ALL_DATES; favoritesOnly.value = false; sort.value = people ? "dateAsc" : "dateDesc";
  } else if (libraryPreferences) {
    const p = libraryPreferences; searchMode.value = p.searchMode; sort.value = p.sort; kind.value = p.kind; dateFilter.value = p.dateFilter; favoritesOnly.value = p.favoritesOnly;
    nextTick(() => { searchInput.value = p.input; query.value = p.query; });
    libraryPreferences = undefined;
  }
}, { immediate: true });

const results = shallowRef<Page<Item>>();
// Only group results from a library request that used the current timeline order.
const loadedLibrarySort = ref<Sort>();
const timelineResultsReady = computed(() => !libraryTimeline.value || loadedLibrarySort.value === activeSort.value);
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
const pageCount = computed(() => Math.ceil((results.value?.total ?? 0) / pageSize));
const pageInput = ref("1");
const pageInputError = ref("");
const paginationDisabled = computed(() => busy.value || itemsLoading.value || searchPending.value || !results.value);
watch(results, (page) => {
  pageInput.value = String((page?.page ?? 0) + 1);
  pageInputError.value = "";
}, { immediate: true });

function jumpToPage() {
  if (paginationDisabled.value) return;
  const input = pageInput.value.trim();
  const page = Number(input);
  if (!/^\d+$/.test(input) || !Number.isSafeInteger(page) || page < 1 || page > pageCount.value) {
    pageInputError.value = `请输入 1 到 ${pageCount.value} 之间的整数页码`;
    return;
  }
  pageInputError.value = "";
  pageInput.value = String(page);
  if (page - 1 !== results.value?.page) requestedPage.value = page - 1;
}

async function fetchPage(scanId: number, currentFilter: typeof filter.value, currentSort: Sort, page: number): Promise<Page<Item>> {
  if (albumsMode.value) {
    if (!selectedAlbum.value) return { page: 0, total: 0, items: [] };
    return invoke<Page<Item>>("album_items", { scanId, albumId: selectedAlbum.value.id, filter: currentFilter, sort: currentSort, page });
  }
  if (peopleMode.value) {
    if (!selectedPerson.value) return { page: 0, total: 0, items: [] };
    return invoke<Page<Item>>("people_photos", { scanId, personId: selectedPerson.value.id, filter: currentFilter, sort: currentSort, page });
  }
  if (searchMode.value === "content") {
    if (!currentFilter.query) return { page: 0, total: 0, items: [] };
    return invoke<Page<Item>>("ai_search", { scanId, query: currentFilter.query, filter: currentFilter, page });
  }
  return invoke<Page<Item>>("library_items", { scanId, filter: currentFilter, sort: currentSort, page });
}
watch([filter, activeSort], () => { requestedPage.value = 0; }, { flush: "sync" });
watch([summary, filter, activeSort, requestedPage, revision, searchMode, aiRevision, () => props.mode, selectedPerson, () => selectedAlbum.value?.id, albumsRevision], async ([scan, currentFilter, currentSort, page], _, onCleanup) => {
  if (!scan) return;
  const requestedLibrarySort = canChooseLibraryView.value ? currentSort : undefined;
  let receiving = true;
  onCleanup(() => { receiving = false; });
  itemsLoading.value = true;
  itemsError.value = "";
  try {
    const result = await fetchPage(scan.scanId, currentFilter, currentSort, page);
    if (receiving) {
      loadedLibrarySort.value = requestedLibrarySort;
      results.value = withSavedRotations(result);
    }
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

watch([summary, selectedPerson, favoritesOnly, locationMode, locationSelection, sort, peopleRevision, () => peopleMode.value], async (_, __, cleanup) => {
  let current = true; cleanup(() => { current = false; });
  const scanId = summary.value?.scanId; const personId = selectedPerson.value?.id;
  months.value = []; timelineError.value = "";
  if (!peopleMode.value || scanId === undefined || personId === undefined) return;
  try {
    const value = await invoke<Month[]>("people_months", { scanId, personId, filter: { query: "", kind: "photo", from: null, to: null, unknownDate: false, favoritesOnly: favoritesOnly.value, locationMode: locationMode.value, locationSelection: locationSelection.value }, sort: sort.value });
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
const detailBody = ref<HTMLElement>();
const detailInformation = ref<HTMLElement>();
const mediaDetails = shallowRef<MediaDetails>();
const mediaDetailsLoading = ref(false);
const mediaDetailsError = ref("");
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
type MetadataRequest = { scanId: number; fileId: number };
let activeMetadata: MetadataRequest | undefined;
let pendingMetadata: MetadataRequest | undefined;
let readingMetadata = false;
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

function selectAlbum(album: Album | undefined) {
  selectedAlbum.value = album;
  requestedPage.value = 0; clearSearch(); kind.value = "all"; dateFilter.value = ALL_DATES;
  favoritesOnly.value = false; sort.value = "dateDesc"; stopSelection();
}
function addToAlbum(album: Album) {
  targetAlbum.value = album; addedAlbum.value = undefined;
  emit("modeChange", "library");
  nextTick(() => { selecting.value = true; });
}
function albumAdded(album: Album) {
  addedAlbum.value = album; targetAlbum.value = undefined;
  clearSelection(); albumsRevision.value++;
}
function returnToAlbum(album: Album) {
  selectAlbum(album); addedAlbum.value = undefined; targetAlbum.value = undefined;
  emit("modeChange", "albums");
}
function albumBusy(value: boolean) { task.value = value ? "saving" : undefined; }
async function removeFromAlbum() {
  const scanId = summary.value?.scanId; const album = selectedAlbum.value;
  if (scanId === undefined || !album || !selectedFiles.value.size || busy.value || !albumsWritable.value) return;
  task.value = "saving"; albumError.value = "";
  try {
    await invoke<AlbumEditResult>("albums_edit", { scanId, edit: { kind: "remove", albumId: album.id, fileIds: [...selectedFiles.value] } });
    if (summary.value?.scanId !== scanId) return;
    clearSelection(); albumsRevision.value++;
  } catch (cause) { if (summary.value?.scanId === scanId) albumError.value = String(cause); }
  finally { task.value = undefined; }
}
watch(() => props.mode, () => { stopSelection(); albumError.value = ""; });
watch(() => props.active, active => {
  if (!active) { stopSelection(); albumPicker.value?.close(); targetAlbum.value = undefined; addedAlbum.value = undefined; }
});

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
    selectedAlbum.value = undefined;
    peopleStatus.value = undefined;
    libraryPreferences = undefined;
    kind.value = "all";
    dateFilter.value = ALL_DATES;
    favoritesOnly.value = false;
    locationMode.value = "all"; locationSelection.value = null; mapFocus.value = null;
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
  targetAlbum.value = undefined; addedAlbum.value = undefined; albumError.value = "";
  albumsWritable.value = false; albumsRevision.value++;
  peopleRevision.value++;
}

async function scanDirectory(selected: string, mode: "select" | "refresh") {
  if (mode === "refresh" && busy.value) return;
  stopSelection();
  albumPicker.value?.close();
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

function prepareDatabaseRestore() {
  closeDetails();
  albumPicker.value?.close();
  selectedAlbum.value = undefined; targetAlbum.value = undefined; addedAlbum.value = undefined; stopSelection();
  albumsWritable.value = false;
  pendingFocus = undefined;
  selectedPerson.value = undefined;
  peopleStatus.value = undefined;
  aiIndex.value = undefined;
  summary.value = undefined;
  results.value = undefined;
  issues.value = undefined;
  months.value = [];
}
function databaseRestored(result: DatabaseRestoreResult) {
  scanError.value = "";
  favoriteError.value = "";
  rotationError.value = "";
  if (result.summary) showScan(result.directory, result.summary);
  else {
    directory.value = result.directory;
    emit("directoryChange", result.directory);
    scanError.value = result.reopenError ?? "请重新打开恢复后的相册。";
  }
}
function reloadAfterRestoreFailure() {
  if (directory.value) void scanDirectory(directory.value, "refresh");
}
function databaseBusy(value: boolean) { task.value = value ? "database" : undefined; }
function databaseExported() { peopleRevision.value++; void aiPanel.value?.refreshIndex(); }

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

watch([summary, filter, activeSort, revision, searchMode, selectedPerson, () => props.mode, selectedAlbum], () => {
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

watch([() => summary.value?.scanId, () => detail.value?.id], ([scanId, fileId]) => {
  activeMetadata = scanId !== undefined && fileId !== undefined ? { scanId, fileId } : undefined;
  pendingMetadata = activeMetadata;
  mediaDetails.value = undefined;
  mediaDetailsError.value = "";
  mediaDetailsLoading.value = !!activeMetadata;
  if (detailBody.value) detailBody.value.scrollTop = 0;
  if (detailInformation.value) detailInformation.value.scrollTop = 0;
  void readLatestMetadata();
});

async function readLatestMetadata() {
  if (readingMetadata) return;
  readingMetadata = true;
  try {
    // Keep at most one request running and skip items already passed during navigation.
    while (pendingMetadata) {
      const request = pendingMetadata;
      pendingMetadata = undefined;
      try {
        const result = await invoke<MediaDetails>("library_media_details", request);
        if (activeMetadata === request) mediaDetails.value = result;
      } catch (cause) {
        if (activeMetadata === request) mediaDetailsError.value = String(cause);
      } finally {
        if (activeMetadata === request) mediaDetailsLoading.value = false;
      }
    }
  } finally {
    readingMetadata = false;
  }
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
      nextPage = await fetchPage(scanId, filter.value, activeSort.value, targetPage);
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
  activeMetadata = undefined;
  pendingMetadata = undefined;
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
        <h1>{{ peopleMode ? "人物与家庭时间线" : albumsMode ? "相册" : "媒体库" }}</h1>
        <p>把每一段回忆，好好珍藏。</p>
      </div>
      <div class="directory-toolbar">
        <div v-if="directory" class="directory-field">
          <label for="library-directory">当前目录</label>
          <ElInput id="library-directory" :model-value="directory" :title="directory" readonly :prefix-icon="FolderOpened" />
        </div>
        <ElButton v-if="directory" :icon="FolderOpened" :disabled="busy" @click="chooseDirectory">更换目录</ElButton>
        <ElButton v-if="directory" :icon="Refresh" :loading="task === 'scanning'" :disabled="busy" @click="scanDirectory(directory, 'refresh')">刷新</ElButton>
        <DatabaseBackup :scan-id="summary?.scanId" :directory="directory" :library-busy="busy"
          @busy-change="databaseBusy" @restoring="prepareDatabaseRestore" @restored="databaseRestored"
          @restore-failed="reloadAfterRestoreFailure" @exported="databaseExported" />
      </div>
    </header>

    <AiSearchSettings v-show="!peopleMode && !albumsMode" ref="aiPanel" :scan-id="summary?.scanId" :library-busy="busy"
      @model-ready="aiReady = $event" @index-change="aiIndex = $event" @updated="aiRevision++" @model-changed="results = undefined; requestedPage = 0; aiRevision++" />

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

    <AlbumsPanel v-if="summary && albumsMode" :scan-id="summary.scanId" :library-busy="busy" :revision="albumsRevision"
      :selected="selectedAlbum" @select="selectAlbum" @changed="albumsRevision++" @add="addToAlbum" @writable="albumsWritable = $event" @busy-change="albumBusy" />
    <AlbumPicker v-if="summary" ref="albumPicker" :scan-id="summary.scanId" :file-ids="[...selectedFiles]" :library-busy="busy"
      :revision="albumsRevision" @added="albumAdded" @busy-change="albumBusy" />

    <template v-if="summary && (peopleMode ? selectedPerson : albumsMode ? selectedAlbum : true)">
      <div v-if="!peopleMode" class="album-selection-tools" role="group" aria-label="批量归类">
        <p v-if="targetAlbum && !albumsMode" role="status">正在为“{{ targetAlbum.name }}”选择照片和视频。 <button type="button" class="secondary" :disabled="busy" @click="targetAlbum = undefined">取消目标</button></p>
        <p v-if="addedAlbum" role="status">已加入“{{ addedAlbum.name }}”。 <button type="button" class="secondary" :disabled="busy" @click="returnToAlbum(addedAlbum!)">返回该相册</button></p>
        <div class="selection-actions">
          <button type="button" class="secondary" :aria-pressed="selecting" :disabled="busy" @click="selecting ? stopSelection() : selecting = true">{{ selecting ? '退出批量选择' : '批量选择' }}</button>
          <template v-if="selecting">
            <span role="status">已选 {{ selectedFiles.size }} 项</span>
            <button type="button" class="secondary" :disabled="busy || itemsLoading || searchPending || !results?.items.length" @click="selectPage(results?.items.map(item => item.id) ?? [])">全选当前页</button>
            <button type="button" class="secondary" :disabled="busy || !selectedFiles.size" @click="clearSelection">清空</button>
            <button type="button" :disabled="busy || itemsLoading || searchPending || !selectedFiles.size" @click="albumPicker?.open(targetAlbum)">加入相册</button>
            <button v-if="albumsMode" type="button" class="secondary" :disabled="busy || !albumsWritable || !selectedFiles.size" @click="removeFromAlbum">从此相册移出</button>
          </template>
        </div>
        <p v-if="albumError" class="error" role="alert">{{ albumError }}</p>
      </div>
      <p v-if="peopleMode" class="timeline-intro">按拍摄日期浏览成长记录，未知日期单独列出。</p>
      <nav v-if="peopleMode && months?.length" class="timeline-months" aria-label="成长时间线年月导航">
        <button type="button" class="secondary" :aria-pressed="dateFilter.mode === 'all'" @click="dateFilter = ALL_DATES">全部日期</button>
        <button v-for="entry in months" :key="entry.month ?? 'unknown'" type="button" class="secondary"
          :aria-pressed="monthSelected(entry.month)" @click="selectMonth(entry.month)">{{ monthTitle(entry.month) }} · {{ entry.count }}</button>
      </nav>
      <p v-if="timelineError" class="error" role="alert">{{ timelineError }}</p>
      <div class="library-tools" :class="{ 'people-tools': peopleMode }" role="search" aria-label="媒体搜索与筛选">
        <div v-if="!peopleMode" class="search-row">
          <ElSelect v-if="!albumsMode" v-model="searchMode" class="mode-select" aria-label="搜索方式">
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
          <DateFilterPicker v-if="active" :key="`${mode}:${directory}:${peopleMode ? selectedPerson?.id : selectedAlbum?.id}`"
            :model-value="dateFilter" @update:model-value="setDateFilter" />
          <div v-if="!albumsMode" class="filter-field">
            <span>拍摄位置</span>
            <ElSelect v-model="locationMode" class="location-select" aria-label="拍摄位置">
              <ElOption label="全部位置" value="all" /><ElOption label="有位置" value="known" /><ElOption label="位置未知" value="unknown" />
            </ElSelect>
          </div>
          <span v-if="!albumsMode && locationLabel" class="location-selection">{{ locationLabel }} <button type="button" class="secondary" @click="locationSelection = null">清除</button></span>
          <ElCheckbox v-model="favoritesOnly" class="favorite-filter">仅收藏</ElCheckbox>
          <div v-if="canChooseLibraryView" class="filter-field">
            <span id="library-view-label">显示方式</span>
            <div class="library-view-options" role="group" aria-labelledby="library-view-label">
              <button type="button" class="secondary" :aria-pressed="libraryView === 'grid'" :disabled="busy" @click="setLibraryView('grid')">网格</button>
              <button type="button" class="secondary" :aria-pressed="libraryTimeline" :disabled="busy" @click="setLibraryView('timeline')">时间轴</button>
            </div>
          </div>
          <div v-if="searchMode === 'filename'" class="filter-field sort-field">
            <span id="sort-label">排序</span>
            <ElSelect v-model="activeSort" class="sort-select" aria-label="排序">
              <ElOption label="拍摄日期：从新到旧" value="dateDesc" /><ElOption label="拍摄日期：从旧到新" value="dateAsc" />
              <template v-if="!peopleMode && !libraryTimeline">
                <ElOption label="名称：升序" value="nameAsc" /><ElOption label="名称：降序" value="nameDesc" />
                <ElOption label="文件大小：从大到小" value="sizeDesc" /><ElOption label="文件大小：从小到大" value="sizeAsc" />
              </template>
            </ElSelect>
          </div>
          <span v-else class="relevance-note">按内容相关度排序 · 最多 200 张</span>
        </div>
      </div>

      <LocationMap v-if="!albumsMode" :scan-id="summary.scanId" :filter="filter" :person-id="peopleMode ? selectedPerson?.id ?? null : null"
        :content-query="!peopleMode && searchMode === 'content' ? query : null" :revision="revision + peopleRevision + aiRevision" :focus="mapFocus" @select="selectLocation" />

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
          <template v-else-if="albumsMode">共 {{ results.total }} 个媒体文件</template>
          <template v-else>{{ results.total === summary.total ? `共 ${summary.total} 个媒体文件` : `${results.total} / ${summary.total} 个媒体文件` }}</template>
        </template>
      </p>
      <div v-if="itemsLoading && (!timelineResultsReady || !results?.items.length)" class="library-empty" role="status">
        <ElIcon class="empty-icon is-loading" aria-hidden="true"><Loading /></ElIcon>
        <h2>正在加载媒体…</h2>
        <p>精彩回忆，即将呈现。</p>
      </div>
      <div v-else-if="!itemsLoading && !itemsError && results?.total === 0 && albumsMode" class="library-empty" role="status">
        <h2>{{ query || kind !== 'all' || dateFilter.mode !== 'all' || favoritesOnly ? '没有找到匹配的照片或视频' : '这个相册还没有照片或视频' }}</h2>
        <p>到媒体库勾选同一次出游或情景里的照片和视频，加入这个相册。</p>
        <button type="button" :disabled="busy || !albumsWritable" @click="selectedAlbum && addToAlbum(selectedAlbum)">去媒体库添加</button>
        <button v-if="query || kind !== 'all' || dateFilter.mode !== 'all' || favoritesOnly" type="button" class="secondary" @click="clearSearch(); kind = 'all'; dateFilter = ALL_DATES; favoritesOnly = false">清除搜索与筛选</button>
      </div>
      <div v-else-if="!itemsLoading && !itemsError && results?.total === 0" class="library-empty" role="status">
        <ElIcon class="empty-icon" aria-hidden="true"><component :is="searchMode === 'content' && !query ? Search : Picture" /></ElIcon>
        <h2>{{ searchMode === 'content' && !query ? '用一句话，找回那个瞬间' : summary.total ? '没有找到匹配的照片或视频' : '这里还没有照片或视频' }}</h2>
        <p>{{ searchMode === 'content' && !query ? '输入照片描述，按回车或点击“搜索照片”。' : summary.total ? '试试其他关键词，或调整类型、日期、位置和收藏筛选。' : '在目录中添加媒体文件后，点击刷新。' }}</p>
        <ElButton v-if="summary.total && (query || kind !== 'all' || dateFilter.mode !== 'all' || favoritesOnly || locationMode !== 'all' || locationSelection)"
          @click="clearSearch(); kind = 'all'; dateFilter = ALL_DATES; favoritesOnly = false; locationMode = 'all'; locationSelection = null">清除搜索与筛选</ElButton>
        <ElButton v-else-if="!summary.total" :icon="Refresh" :disabled="busy" @click="scanDirectory(directory, 'refresh')">刷新目录</ElButton>
      </div>

      <ul v-if="timelineResultsReady && results?.items.length" ref="grid" class="media-grid" :aria-busy="itemsLoading">
        <template v-for="(item, index) in results.items" :key="`${summary.scanId}:${item.id}`">
        <li v-if="(peopleMode || libraryTimeline) && (index === 0 || item.capturedOn?.slice(0, 7) !== results.items[index - 1]?.capturedOn?.slice(0, 7))" class="timeline-heading">
          <h2>{{ monthTitle(item.capturedOn) }}</h2>
        </li>
        <li class="card" :class="{ 'card-selected': selecting && selectedFiles.has(item.id) }">
          <label v-if="selecting && !peopleMode" class="media-select"><input type="checkbox" :checked="selectedFiles.has(item.id)" :disabled="busy || itemsLoading || searchPending" :aria-label="`选择 ${item.name}`" @change="toggleSelection(item.id)" />选择</label>
          <button type="button" class="media-preview secondary" :data-file-id="item.id" :disabled="busy || itemsLoading || searchPending"
            :aria-label="`查看 ${item.name}`" @click="showDetails(item, $event)">
            <LibraryThumbnail v-if="item.previewAvailable" :scan-id="summary.scanId"
              :file-id="item.id" :video="item.kind === 'video'" :rotation="item.rotation" />
            <span v-else class="media-placeholder"><ElIcon aria-hidden="true"><component :is="item.kind === 'video' ? VideoPlay : Picture" /></ElIcon>{{ item.kind === "video" ? "视频" : "照片" }}</span>
          </button>
          <div class="card-details">
            <div class="card-heading">
              <p class="file-name" :title="item.relativePath">{{ item.name }}</p>
              <button type="button" class="favorite secondary" :aria-label="`${item.favorite ? '取消收藏' : '收藏'} ${item.name}`"
                :title="item.favorite ? '取消收藏' : '收藏'" :aria-pressed="item.favorite"
                :disabled="busy || itemsLoading || searchPending" @click="toggleFavorite(item)"><ElIcon aria-hidden="true"><component :is="item.favorite ? StarFilled : Star" /></ElIcon></button>
            </div>
            <div class="card-info">
              <span :title="item.capturedOn ?? '日期未知'">{{ item.capturedOn ?? "日期未知" }}</span>
              <span class="card-location" :title="item.location?.address ?? shortLocation(item.location)">{{ shortLocation(item.location) }}</span>
            </div>
          </div>
        </li>
        </template>
      </ul>
      <nav v-if="results && results.total > pageSize" class="pagination" aria-label="媒体列表分页">
        <button type="button" class="secondary" :disabled="paginationDisabled || results.page === 0"
          @click="requestedPage = results.page - 1">上一页</button>
        <span>{{ results.page + 1 }} / {{ pageCount }}</span>
        <button type="button" class="secondary" :disabled="paginationDisabled || (results.page + 1) * pageSize >= results.total"
          @click="requestedPage = results.page + 1">下一页</button>
        <form class="page-jump" novalidate @submit.prevent="jumpToPage">
          <label class="page-jump-label">跳至第
            <input v-model="pageInput" type="text" inputmode="numeric" aria-label="跳转页码" autocomplete="off"
              :disabled="paginationDisabled" :aria-invalid="!!pageInputError"
              :aria-describedby="pageInputError ? 'library-page-error' : undefined" @input="pageInputError = ''" />
            页
          </label>
          <button type="submit" class="secondary" :disabled="paginationDisabled">跳转</button>
        </form>
        <p v-if="pageInputError" id="library-page-error" class="page-jump-error error" role="alert">{{ pageInputError }}</p>
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

    <dialog v-if="detail" ref="dialog" class="media-detail" aria-labelledby="library-preview-title"
      @cancel.prevent="closeDetails" @close="finishDetails" @keydown="handleDetailKey">
      <header class="detail-header">
        <h2 id="library-preview-title" :title="detail.name">{{ detail.name }}</h2>
        <button type="button" class="secondary" @click="closeDetails">关闭</button>
      </header>
      <nav class="detail-navigation" aria-label="连续浏览">
        <button type="button" class="secondary" :disabled="detailNavigating || rotationSaving || detailPosition === 0" @click="moveDetail(-1)">上一项</button>
        <span role="status">第 {{ detailPosition + 1 }} / {{ detailPage?.total ?? 0 }} 项</span>
        <button type="button" class="secondary" :disabled="detailNavigating || rotationSaving || detailPosition + 1 >= (detailPage?.total ?? 0)" @click="moveDetail(1)">下一项</button>
      </nav>
      <div ref="detailBody" class="detail-body">
        <div class="detail-viewer">
          <p v-if="detailNavigating" class="status" role="status">正在切换媒体…</p>
          <video v-else-if="liveVideoUrl" ref="liveVideo" class="detail-image" :src="liveVideoUrl"
            :aria-label="detail.name" controls playsinline
            @error="handleLiveVideoError" />
          <RotatableImage v-else-if="detailImage" :src="detailImage" :alt="detail.name" :rotation="detail.rotation" detail
            @error="previewReady = false; previewError = '无法显示图片预览。'" />
          <p v-else-if="detail.previewAvailable && !previewError" class="status" role="status">正在加载预览…</p>
          <p v-if="previewError" class="error" role="alert">{{ previewError }}</p>
          <p v-else-if="!detail.previewAvailable && !detailNavigating" class="viewer-placeholder">此媒体暂不支持预览，可用系统默认应用打开。</p>
        </div>
        <aside ref="detailInformation" class="detail-information" aria-label="媒体信息与操作">
          <div class="information-header">
            <h3>{{ detail.isLivePhoto ? '实况照片信息' : detail.kind === 'video' ? '视频信息' : '照片信息' }}</h3>
            <button v-if="detail.isLivePhoto && !liveVideoUrl" type="button" :disabled="detailNavigating || rotationSaving || liveVideoLoading"
              @click="playLivePhoto">
              {{ liveVideoLoading ? "正在加载实况视频…" : "播放实况" }}
            </button>
          </div>
          <p v-if="liveVideoError" class="error" role="alert">{{ liveVideoError }}</p>
          <div v-if="detail.kind === 'photo' && detail.previewAvailable && !liveVideoUrl" class="rotation-actions">
            <button type="button" class="secondary" :disabled="!canRotate" @click="rotateDetail(-90)">向左旋转 90°</button>
            <button type="button" class="secondary" :disabled="!canRotate" @click="rotateDetail(90)">向右旋转 90°</button>
            <span class="status" role="status">{{ rotationSaving ? "正在保存方向…" : `已保存方向：${detail.rotation}°` }}</span>
          </div>
          <p v-if="rotationError" class="error" role="alert">{{ rotationError }}</p>
          <MediaDetailsInfo :item="detail" :details="mediaDetails" :loading="mediaDetailsLoading" :error="mediaDetailsError" />
          <section class="detail-location">
            <h4>位置</h4>
            <p>{{ detail.location?.address || (detail.location ? '地址暂不可用' : '位置未知') }}</p>
            <template v-if="detail.location">
              <p>纬度 {{ detail.location.latitude.toFixed(6) }} · 经度 {{ detail.location.longitude.toFixed(6) }}（WGS84）</p>
              <button v-if="!albumsMode" type="button" class="secondary" @click="locateDetail">在地图中查看</button>
            </template>
          </section>
          <p v-if="detailPeople.length" class="status">照片中的人物：{{ detailPeople.join("、") }}</p>
          <details v-if="summary && detail.kind === 'photo'" class="detail-people">
            <summary>照片中的人物 / 修改分组</summary>
            <PeopleFaceEditor :scan-id="summary.scanId" :file-id="detail.id" :revision="peopleRevision" :writable="peopleStatus?.writable" :busy="busy || rotationSaving"
              @changed="detailPeopleChanged" />
          </details>
          <dl class="detail-file-path">
            <dt>相对路径</dt>
            <dd>{{ detail.relativePath }}</dd>
          </dl>
          <div class="detail-actions">
            <button type="button" :disabled="!!fileAction || detailNavigating" @click="performFileAction('open')">
              {{ detail.kind === "video" ? "用系统播放器打开" : "用系统默认应用打开" }}
            </button>
            <button type="button" class="secondary" :disabled="!!fileAction || detailNavigating" @click="performFileAction('reveal')">在文件管理器中显示</button>
            <button type="button" class="secondary" :disabled="!!fileAction || detailNavigating" @click="performFileAction('copy')">复制完整路径</button>
          </div>
          <p v-if="detailStatus" class="status" role="status">{{ detailStatus }}</p>
          <p v-if="detailError" class="error" role="alert">{{ detailError }}</p>
        </aside>
      </div>
    </dialog>
  </div>
</template>

<style scoped>
.album-selection-tools { margin-block: 16px; padding: 16px; border: 1px solid var(--border); border-radius: 10px; background: var(--surface); }
.selection-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; }
.media-select { display: flex; align-items: center; gap: 6px; padding: 8px 2px; }
.card-selected { outline: 2px solid var(--accent); outline-offset: 4px; border-radius: 10px; }

.media-library { color: var(--text); --library-preview: var(--preview); }
.library-header { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 24px; margin-bottom: 28px; }
.library-heading { flex: 1 0 160px; }
.library-heading h1 { font-size: 28px; }
.library-heading p { margin: 8px 0 0; font-size: 13px; color: var(--muted); }
.directory-toolbar { display: flex; flex-wrap: wrap; flex: 0 1 720px; align-items: end; gap: 8px; min-width: 0; max-width: 100%; }
.directory-field { flex: 1 1 200px; min-width: 0; }
.directory-field label { margin: 0 0 4px; font-size: 12px; font-weight: 400; color: var(--muted); }
.directory-field :deep(input) { text-overflow: ellipsis; }
.library-tools { margin-top: 20px; padding: 0 0 20px; border-bottom: 1px solid var(--border); }
.search-row { display: flex; gap: 8px; min-width: 0; }
.mode-select { flex: 0 0 120px; }
.search-input { flex: 1; min-width: 0; }
.filters { display: flex; flex-wrap: wrap; align-items: center; gap: 12px 20px; margin-top: 16px; }
.filter-field { display: flex; align-items: center; gap: 8px; min-width: 0; }
.filter-field > span { color: var(--muted); white-space: nowrap; font-size: 13px; }
.library-view-options { display: inline-flex; gap: 4px; }
.library-view-options button { min-height: 32px; padding: 4px 10px; white-space: nowrap; font-size: 13px; }
.library-view-options button[aria-pressed="true"]:not(:disabled) { border-color: var(--accent); background: var(--accent-soft); color: var(--accent); }
.kind-select, .location-select { width: 120px; }
.sort-select { width: 200px; }
.sort-field { margin-left: auto; }
.favorite-filter { margin-right: 0; }
.relevance-note { margin-left: auto; color: var(--muted); font-size: 13px; }
.location-selection { display: inline-flex; flex-wrap: wrap; align-items: center; gap: 8px; color: var(--accent); font-size: 13px; }
.scan-status, .library-notice { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; margin: 16px 0; padding: 12px 16px; border-radius: 8px; background: var(--accent-soft); color: var(--accent); }
.library-notice { background: var(--danger-soft); color: var(--danger); }
.library-notice span { flex: 1; min-width: 0; overflow-wrap: anywhere; }
.library-empty { display: grid; justify-items: center; gap: 16px; margin-top: 24px; padding: clamp(40px, 8vw, 100px) 20px; border-radius: 12px; background: var(--surface); text-align: center; }
.library-empty h2 { font-size: 22px; }
.library-empty p { margin: 0; max-width: 440px; color: var(--muted); }
.empty-icon { width: 72px; height: 72px; margin-bottom: 8px; border-radius: 20px; background: var(--accent-soft); color: var(--accent); font-size: 32px; }
.empty-hint { font-size: 12px; color: var(--muted); }
.result-status { margin: 20px 0 12px; font-size: 13px; }
.result-status:empty { margin: 0; }
.media-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 180px), 1fr)); gap: 20px 12px; padding: 0; margin: 0; list-style: none; }
.card { min-width: 0; border-radius: 10px; }
button.media-preview { display: grid; width: 100%; aspect-ratio: 4 / 3; grid-template: minmax(0, 1fr) / minmax(0, 1fr); place-items: center; padding: 0; overflow: hidden; border: 0; border-radius: 10px; background: var(--preview); color: var(--muted); font-size: 13px; }
button.media-preview:hover:not(:disabled) { background: var(--hover); }
button.media-preview:focus-visible { outline-offset: 3px; }
.card-details { padding: 7px 2px 0; }
.media-placeholder { display: inline-flex; align-items: center; gap: 6px; }
.card-heading { display: flex; align-items: center; gap: 4px; }
.file-name { flex: 1; min-width: 0; margin: 0; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; font-size: 13px; font-weight: 500; }
.card-info { display: flex; align-items: center; justify-content: space-between; gap: 8px; color: var(--muted); font-size: 12px; font-variant-numeric: tabular-nums; }
.card-info > span { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.card-info > span:first-child { flex-shrink: 0; }
.card-location { min-width: 0; text-align: right; }
button.favorite { display: grid; place-items: center; flex: none; width: 32px; min-height: 32px; padding: 0; border: 0; border-radius: 8px; background: transparent; color: var(--muted); font-size: 18px; }
button.favorite[aria-pressed="true"] { color: var(--accent); }
button.favorite:hover:not(:disabled) { background: var(--accent-soft); color: var(--accent); }
.pagination, .detail-navigation, .rotation-actions, .detail-actions, .detail-header { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; }
.pagination { justify-content: center; margin-top: 28px; color: var(--muted); font-size: 13px; }
.page-jump { display: flex; align-items: center; gap: 8px; }
.page-jump .page-jump-label { display: flex; align-items: center; gap: 8px; margin: 0; font-weight: 400; white-space: nowrap; }
.page-jump input { width: 76px; text-align: center; }
.page-jump-error { flex-basis: 100%; margin: 0; text-align: center; }
.timeline-months { display: flex; flex-wrap: wrap; gap: 8px; max-height: 160px; overflow-y: auto; margin: 20px 0; }
.timeline-months button { font-size: 13px; }
.timeline-months [aria-pressed="true"] { background: var(--accent); border-color: var(--accent); color: var(--on-accent); }
.timeline-heading { grid-column: 1 / -1; margin-top: 20px; }
.timeline-intro { color: var(--muted); }
.people-tools .filters { margin-top: 0; }
.media-library > details { margin-top: 24px; }
.media-detail { width: min(1320px, calc(100vw - 48px)); max-width: calc(100vw - 32px); padding: 0; overflow: hidden; }
.detail-header { justify-content: space-between; align-items: center; padding: 16px 24px; margin: 0; }
.detail-header h2 { flex: 1; min-width: 0; font-size: 16px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.detail-navigation { justify-content: center; padding: 0 24px 16px; margin: 0; font-size: 13px; color: var(--muted); }
.detail-navigation span { font-variant-numeric: tabular-nums; }
.detail-body { display: grid; grid-template-columns: minmax(0, 1fr) 360px; height: min(72vh, 820px); border-top: 1px solid var(--border); }
.detail-viewer { display: grid; place-items: center; min-width: 0; min-height: 0; overflow: hidden; background: var(--viewer); color: var(--viewer-text); }
.detail-viewer .status, .viewer-placeholder { padding: 24px; color: var(--viewer-text); text-align: center; }
.detail-viewer .error { padding: 24px; color: var(--viewer-error); }
.detail-viewer :deep(.rotatable-image.detail) { width: 100%; height: 100%; max-height: none; aspect-ratio: auto !important; background: var(--viewer); }
.detail-image { display: block; width: 100%; height: 100%; min-height: 0; object-fit: contain; background: var(--viewer); }
.detail-information { min-width: 0; overflow-y: auto; padding: 24px; border-left: 1px solid var(--border); background: var(--surface); }
.information-header { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 8px; margin-bottom: 14px; }
.information-header h3 { margin: 0; font-size: 16px; font-weight: 650; }
.information-header button { padding: 6px 10px; min-height: 32px; font-size: 13px; }
.detail-location, .detail-information .status { font-size: 13px; }
.detail-location { padding-block: 14px; border-top: 1px solid var(--border); }
.detail-location h4 { margin: 0 0 9px; font-size: 13px; font-weight: 650; }
.detail-location p { margin: 0 0 6px; color: var(--text-secondary); }
.detail-location p:last-child { margin-bottom: 0; }
.rotation-actions { gap: 8px; margin-bottom: 14px; }
.rotation-actions button { flex: 1; padding-inline: 8px; font-size: 12px; }
.rotation-actions .status { flex-basis: 100%; }
.detail-people { margin-top: 16px; padding-block: 16px; border-block: 1px solid var(--border); font-size: 13px; }
.detail-file-path { display: block; margin: 14px 0 0; font-size: 13px; overflow-wrap: anywhere; }
.detail-file-path dd { margin: 4px 0 0; font-size: inherit; font-weight: 500; }
.detail-actions { align-items: stretch; flex-direction: column; gap: 8px; margin-top: 16px; }
@media (max-width: 1199px) {
  .directory-toolbar { flex-basis: 100%; }
  .directory-field { flex-basis: 200px; }
  .sort-field { margin-left: 0; }
}
@media (max-width: 959px) {
  .detail-body { display: flex; flex-direction: column; height: auto; max-height: calc(100dvh - 180px); overflow-y: auto; }
  .detail-viewer { flex: none; height: 50vh; min-height: 240px; }
  .detail-information { flex: none; overflow: visible; border-left: 0; border-top: 1px solid var(--border); }
  .detail-actions { flex-direction: row; }
  .detail-people :deep(.faces) { grid-template-columns: repeat(auto-fill, minmax(140px, 1fr)); }
}
@media (max-width: 639px) {
  .library-header { gap: 20px; }
  .library-heading h1 { font-size: 26px; }
  .directory-field { flex-basis: 100%; }
  .directory-toolbar { gap: 8px; }
  .directory-toolbar :deep(.el-button) { padding-inline: 12px; }
  .search-row { flex-wrap: wrap; }
  .mode-select { flex-basis: 110px; }
  .search-input { flex-basis: calc(100% - 118px); }
  .search-row > .el-button { width: 100%; }
  .filters { gap: 12px; }
  .filter-field { flex: 1 1 150px; justify-content: space-between; }
  .filter-field :deep(.el-select) { flex: 1; min-width: 0; width: auto; }
  .sort-field { flex-basis: 100%; }
  .relevance-note { margin-left: 0; }
  .media-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  button.favorite { width: 40px; min-height: 40px; }
  .media-detail { width: calc(100vw - 24px); max-width: calc(100vw - 24px); }
  .detail-header { padding: 12px 16px; }
  .detail-navigation { gap: 8px; padding: 0 12px 12px; }
  .detail-navigation button { padding-inline: 10px; }
  .detail-information { padding: 20px 16px; }
}
@media (max-width: 359px) { .media-grid { grid-template-columns: minmax(0, 1fr); } }
</style>
