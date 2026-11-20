<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, nextTick, ref, shallowRef, watch } from "vue";
import SimilarPhotoDetails from "./SimilarPhotoDetails.vue";
import SimilarPhotoPreview from "./SimilarPhotoPreview.vue";
import { formatBytes } from "./formatBytes";

type MatchKind = "variant" | "burst";
type Progress =
  | { phase: "scanning"; found: number }
  | { phase: "extracting" | "matching"; processed: number; total: number };

interface Summary {
  scanId: number;
  found: number;
  checked: number;
  reused: number;
  unsupported: number;
  skipped: number;
  groups: number;
  cacheWarning: string | null;
}

type Outcome = { status: "finished"; summary: Summary } | { status: "cancelled" };

interface Photo {
  id: number;
  name: string;
  relativePath: string;
  size: number;
  width: number;
  height: number;
  capturedAt: string | null;
  hardLink: boolean;
}

interface Group {
  id: number;
  representative: Photo;
  count: number;
  hasVariant: boolean;
  hasBurst: boolean;
}

interface Member {
  photo: Photo;
  kind: MatchKind | null;
}

interface Issue {
  path: string;
  reason: string;
}

interface Page<T> {
  total: number;
  items: T[];
}

interface TrashOutcome {
  id: number;
  relativePath: string;
  status: "moved" | "skipped" | "failed";
  reason: string | null;
}

interface TrashResult {
  outcomes: TrashOutcome[];
  totalGroups: number;
  affectedGroups: Group[];
}

interface TrashRequest {
  mode: "single" | "batch";
  photos: Photo[];
}

interface GroupRefresh {
  mode: TrashRequest["mode"];
  groupId: number;
  affectedIds: number[];
  keepComparison: boolean;
}

const pageSize = 25;
const compareLimit = 4;
const labels = { variant: "同图变体", burst: "疑似连拍" };
const emit = defineEmits<{ busyChange: [busy: boolean] }>();
const directory = ref("");
const choosing = ref(false);
const activeScan = ref<number>();
const cancelling = ref(false);
const trashing = ref(false);
const refreshingGroups = ref(false);
const busy = computed(() => choosing.value || activeScan.value !== undefined || trashing.value || refreshingGroups.value);
watch(busy, (value) => emit("busyChange", value), { flush: "sync" });

const progress = shallowRef<Progress>();
const summary = shallowRef<Summary>();
const scanId = computed(() => summary.value?.scanId);
const scanError = ref("");
const notice = ref("");

const filter = ref<MatchKind | "all">("all");
const groupPage = ref(0);
const groups = shallowRef<Page<Group>>();
const groupError = ref("");
const expanded = shallowRef<Group>();
const memberPage = ref(0);
const members = shallowRef<Page<Member>>();
const memberError = ref("");
const compared = shallowRef<Member[]>([]);
const comparedIds = computed(() => new Set(compared.value.map(({ photo }) => photo.id)));
const viewing = shallowRef<{ member: Member; trigger: HTMLButtonElement }>();
let fillComparisonTo: number | undefined;
const selected = shallowRef(new Map<number, Photo>());
const selectedBytes = computed(() => [...selected.value.values()].reduce((total, photo) => total + photo.size, 0));
const confirmation = ref<HTMLDialogElement>();
const trashRequest = shallowRef<TrashRequest>();
const confirmationBytes = computed(() => trashRequest.value?.photos.reduce((total, photo) => total + photo.size, 0) ?? 0);
const resultsSection = ref<HTMLElement>();
const outcomes = shallowRef<TrashOutcome[]>([]);
const trashError = ref("");
const singleTrashError = ref("");
const revision = ref(0);
let refreshGroup: GroupRefresh | undefined;
const statusLabels = { moved: "已移入回收站", skipped: "已跳过", failed: "失败" };
const outcomeCounts = computed(() => {
  const counts = { moved: 0, skipped: 0, failed: 0 };
  for (const outcome of outcomes.value) counts[outcome.status]++;
  return counts;
});

const issuesOpen = ref(false);
const issuePage = ref(0);
const issues = shallowRef<Page<Issue>>();
const issueError = ref("");

function closeGroup() {
  viewing.value = undefined;
  selected.value = new Map();
  confirmation.value?.close();
  trashRequest.value = undefined;
  expanded.value = undefined;
  members.value = undefined;
  memberPage.value = 0;
  compared.value = [];
  fillComparisonTo = undefined;
  singleTrashError.value = "";
}

function resetResults() {
  outcomes.value = [];
  trashError.value = "";
  refreshGroup = undefined;
  summary.value = undefined;
  filter.value = "all";
  groupPage.value = 0;
  issuesOpen.value = false;
  issuePage.value = 0;
  closeGroup();
}

function useDirectory(folder: string) {
  if (trashing.value || directory.value === folder) return;
  directory.value = folder;
  scanError.value = "";
  notice.value = "";
  resetResults();
}

defineExpose({ useDirectory });

async function chooseDirectory() {
  if (busy.value) return;
  choosing.value = true;
  scanError.value = "";
  try {
    const folder = await invoke<string | null>("choose_folder", { title: "选择相似照片扫描目录" });
    if (folder !== null) useDirectory(folder);
  } catch (cause) {
    scanError.value = `无法选择目录。${String(cause)}`;
  } finally {
    choosing.value = false;
  }
}

async function startScan() {
  if (!directory.value || busy.value) return;
  resetResults();
  const id = Date.now();
  activeScan.value = id;
  scanError.value = "";
  notice.value = "";
  try {
    const channel = new Channel<Progress>((update) => {
      if (activeScan.value === id) progress.value = update;
    });
    const outcome = await invoke<Outcome>("scan_similar", { directory: directory.value, scanId: id, progress: channel });
    if (outcome.status === "finished") summary.value = outcome.summary;
    else notice.value = "扫描已取消。";
  } catch (cause) {
    scanError.value = String(cause);
  } finally {
    activeScan.value = undefined;
    progress.value = undefined;
    cancelling.value = false;
  }
}

async function cancelScan() {
  const id = activeScan.value;
  if (id === undefined || cancelling.value) return;
  cancelling.value = true;
  try {
    await invoke("cancel_similar", { scanId: id });
  } catch (cause) {
    if (activeScan.value === id) {
      cancelling.value = false;
      scanError.value = `取消失败。${String(cause)}`;
    }
  }
}

function toggleGroup(group: Group) {
  if (busy.value) return;
  const wasOpen = expanded.value?.id === group.id;
  closeGroup();
  if (!wasOpen) openGroup(group);
}

function openGroup(group: Group) {
  fillComparisonTo = compareLimit;
  expanded.value = group;
}

function comparisonDisabled(photo: Photo) {
  return busy.value || (!comparedIds.value.has(photo.id) && compared.value.length >= compareLimit);
}

function comparisonSection() {
  return resultsSection.value?.querySelector<HTMLElement>("#similar-comparison");
}

function openPhotoDetails(member: Member, event: MouseEvent) {
  if (busy.value || viewing.value) return;
  viewing.value = { member, trigger: event.currentTarget as HTMLButtonElement };
}

function closePhotoDetails() {
  const trigger = viewing.value?.trigger;
  viewing.value = undefined;
  trigger?.focus({ preventScroll: true });
}

async function toggleComparison(member: Member, fromCard = false) {
  if (comparisonDisabled(member.photo)) return;
  compared.value = comparedIds.value.has(member.photo.id)
    ? compared.value.filter(({ photo }) => photo.id !== member.photo.id)
    : [...compared.value, member];
  if (fromCard) {
    await nextTick();
    comparisonSection()?.focus();
  }
}

function selectionDisabled(photo: Photo) {
  return busy.value || photo.hardLink ||
    (!selected.value.has(photo.id) && selected.value.size >= (expanded.value?.count ?? 0) - 1);
}

function toggleSelection(photo: Photo) {
  if (selectionDisabled(photo)) return;
  const selection = new Map(selected.value);
  if (selection.has(photo.id)) selection.delete(photo.id);
  else selection.set(photo.id, photo);
  selected.value = selection;
}

function canTrashOne(photo: Photo) {
  return !busy.value && !photo.hardLink && (expanded.value?.count ?? 0) > 1
    && comparedIds.value.has(photo.id);
}

function openTrashConfirmation(photo?: Photo) {
  const dialog = confirmation.value;
  if (!dialog || dialog.open || busy.value || !expanded.value) return;
  if (photo ? !canTrashOne(photo) : !selected.value.size) return;
  trashRequest.value = photo
    ? { mode: "single", photos: [photo] }
    : { mode: "batch", photos: [...selected.value.values()] };
  dialog.showModal();
}

function clearTrashRequest() {
  if (!confirmation.value?.open) trashRequest.value = undefined;
}

async function focusTrashResult(single: boolean) {
  await nextTick();
  (single ? comparisonSection() ?? resultsSection.value : resultsSection.value)
    ?.focus({ preventScroll: !single });
}

async function trashPhotos() {
  const current = summary.value;
  const group = expanded.value;
  const request = trashRequest.value;
  if (!current || !group || !request || busy.value || !confirmation.value?.open) return;
  const single = request.mode === "single";
  if (single && !canTrashOne(request.photos[0])) return;
  const fileIds = request.photos.map(({ id }) => id);
  confirmation.value.close();
  trashing.value = true;
  trashError.value = "";
  singleTrashError.value = "";
  outcomes.value = [];
  try {
    const result = await invoke<TrashResult>("trash_similar", { scanId: current.scanId, groupId: group.id, fileIds });
    summary.value = { ...current, groups: result.totalGroups };
    outcomes.value = result.outcomes;
    if (single) {
      const outcome = result.outcomes.find(({ id }) => id === fileIds[0]);
      if (outcome?.status !== "moved") {
        singleTrashError.value = outcome?.reason || "照片未移入回收站，请重试。";
        return;
      }
    }

    const remainingGroup = result.affectedGroups.find(({ id }) => id === group.id);
    const keepComparison = single && !!remainingGroup && fileIds[0] !== group.representative.id;
    if (keepComparison) {
      fillComparisonTo = compared.value.length;
      compared.value = compared.value.filter(({ photo }) => photo.id !== fileIds[0]);
      const selection = new Map(selected.value);
      selection.delete(fileIds[0]);
      if (selection.size === remainingGroup.count) selection.delete(remainingGroup.representative.id);
      selected.value = selection;
      if (members.value) {
        members.value = {
          total: remainingGroup.count,
          items: members.value.items.filter(({ photo }) => photo.id !== fileIds[0]),
        };
      }
    } else {
      closeGroup();
    }
    if (!result.totalGroups) return;
    refreshGroup = {
      mode: request.mode,
      groupId: group.id,
      affectedIds: result.affectedGroups.map(({ id }) => id),
      keepComparison,
    };
    refreshingGroups.value = true;
    revision.value++;
  } catch (cause) {
    if (single) singleTrashError.value = String(cause);
    else trashError.value = String(cause);
  } finally {
    trashing.value = false;
    if (!refreshingGroups.value) await focusTrashResult(single);
  }
}

function pageCount(total: number) {
  return Math.max(1, Math.ceil(total / pageSize));
}

function changeGroupPage(page: number) {
  if (busy.value) return;
  refreshGroup = undefined;
  closeGroup();
  groupPage.value = page;
}

watch(filter, () => changeGroupPage(0), { flush: "sync" });
watch([scanId, filter, groupPage, revision], async ([id, kind, page], _, onCleanup) => {
  const refresh = refreshGroup;
  if (refresh) refreshingGroups.value = true;
  else groups.value = undefined;
  groupError.value = "";
  if (id === undefined) return;
  let current = true;
  onCleanup(() => { current = false; });
  try {
    const result = await invoke<Page<Group>>("similar_groups", { scanId: id, kind: kind === "all" ? null : kind, page });
    if (current) {
      const lastPage = pageCount(result.total) - 1;
      if (page > lastPage) {
        groupPage.value = lastPage;
        return;
      }
      groups.value = result;
      if (refresh) {
        const group = result.items.find(({ id }) => id === refresh.groupId)
          ?? result.items.find(({ id }) => refresh.affectedIds.includes(id))
          ?? (refresh.mode === "batch" ? result.items[0] : undefined);
        if (group && refresh.keepComparison && group.id === refresh.groupId) {
          memberPage.value = Math.min(memberPage.value, pageCount(group.count) - 1);
          expanded.value = group;
        } else {
          closeGroup();
          if (group) openGroup(group);
        }
        refreshGroup = undefined;
        refreshingGroups.value = false;
        await focusTrashResult(refresh.mode === "single");
      }
    }
  } catch (cause) {
    if (current) {
      groupError.value = String(cause);
      refreshingGroups.value = false;
      if (refresh) await focusTrashResult(refresh.mode === "single");
    }
  }
});

watch([scanId, expanded, memberPage], async ([id, group, page], _, onCleanup) => {
  members.value = undefined;
  memberError.value = "";
  if (id === undefined || !group) return;
  let current = true;
  onCleanup(() => { current = false; });
  try {
    const result = await invoke<Page<Member>>("similar_members", { scanId: id, groupId: group.id, page });
    if (current) {
      members.value = result;
      if (fillComparisonTo !== undefined) {
        const available = result.items.filter(({ photo }) => !comparedIds.value.has(photo.id));
        const slots = Math.max(0, fillComparisonTo - compared.value.length);
        compared.value = [...compared.value, ...available.slice(0, slots)];
        fillComparisonTo = undefined;
      }
    }
  } catch (cause) {
    if (current) memberError.value = String(cause);
  }
});

watch([scanId, issuesOpen, issuePage], async ([id, open, page], _, onCleanup) => {
  issues.value = undefined;
  issueError.value = "";
  if (id === undefined || !open) return;
  let current = true;
  onCleanup(() => { current = false; });
  try {
    const result = await invoke<Page<Issue>>("similar_issues", { scanId: id, page });
    if (current) issues.value = result;
  } catch (cause) {
    if (current) issueError.value = String(cause);
  }
});
</script>

<template>
  <div>
    <form @submit.prevent="startScan">
      <label for="similar-dir">扫描目录</label>
      <div class="folder-row">
        <input id="similar-dir" :value="directory" placeholder="尚未选择扫描目录" readonly />
        <button type="button" class="secondary" :disabled="busy" @click="chooseDirectory">选择目录</button>
      </div>
      <div class="actions">
        <button type="submit" :disabled="busy || !directory">{{ summary ? "重新扫描" : "开始扫描" }}</button>
        <button v-if="activeScan !== undefined" type="button" class="secondary"
          :disabled="!progress || cancelling" @click="cancelScan">
          {{ cancelling ? "正在取消…" : "取消扫描" }}
        </button>
      </div>

      <div v-if="activeScan !== undefined" class="archive-progress" role="status">
        <template v-if="!progress || progress.phase === 'scanning'">
          <p>正在读取目录 · 已发现 {{ progress?.found ?? 0 }} 张照片</p>
          <progress aria-label="读取目录进度"></progress>
        </template>
        <template v-else>
          <p>{{ progress.phase === "extracting" ? "正在提取图像特征" : "正在查找相似组" }} {{ progress.processed }} / {{ progress.total }}</p>
          <progress :value="progress.processed" :max="Math.max(1, progress.total)" aria-label="相似照片扫描进度"></progress>
        </template>
      </div>
      <p v-if="notice" role="status">{{ notice }}</p>
      <p v-if="scanError" class="error" role="alert">{{ scanError }}</p>
      <p v-if="trashing" role="status">正在核验照片并移到回收站…</p>
    </form>

    <section v-if="summary" ref="resultsSection" tabindex="-1" aria-labelledby="similar-results" :aria-busy="trashing || refreshingGroups">
      <h2 id="similar-results">扫描结果</h2>
      <dl aria-live="polite">
        <div><dt>成功检查</dt><dd>{{ summary.checked }}</dd></div>
        <div><dt>复用缓存</dt><dd>{{ summary.reused }}</dd></div>
        <div><dt>跳过项</dt><dd>{{ summary.skipped }}</dd></div>
        <div><dt>相似组</dt><dd>{{ summary.groups }}</dd></div>
      </dl>
      <p v-if="summary.cacheWarning" role="status">{{ summary.cacheWarning }}</p>
      <p v-if="summary.unsupported">跳过了 {{ summary.unsupported }} 张格式暂不支持的照片。</p>
      <p v-if="trashError" class="error" role="alert">{{ trashError }}</p>
      <div v-if="outcomes.length" class="batch-result">
        <p role="status">
          已移入回收站 {{ outcomeCounts.moved }} 张，跳过 {{ outcomeCounts.skipped }} 张，失败 {{ outcomeCounts.failed }} 张。
        </p>
        <details :open="outcomeCounts.skipped + outcomeCounts.failed > 0">
          <summary>处理明细</summary>
          <ul class="duplicate-list">
            <li v-for="outcome in outcomes" :key="outcome.id">
              <strong>{{ outcome.relativePath }}</strong>
              <span>{{ statusLabels[outcome.status] }}<template v-if="outcome.reason"> · {{ outcome.reason }}</template></span>
            </li>
          </ul>
        </details>
      </div>
      <details v-if="summary.skipped" class="duplicate-details"
        @toggle="issuesOpen = ($event.currentTarget as HTMLDetailsElement).open">
        <summary>问题明细（{{ summary.skipped }} 项）</summary>
        <p v-if="issueError" class="error" role="alert">{{ issueError }}</p>
        <template v-else-if="issues">
          <ul class="duplicate-list">
            <li v-for="(issue, index) in issues.items" :key="index">
              <strong>{{ issue.path }}</strong>
              <span>{{ issue.reason }}</span>
            </li>
          </ul>
          <nav v-if="pageCount(issues.total) > 1" class="actions" aria-label="问题明细分页">
            <button type="button" class="secondary" :disabled="busy || issuePage === 0" @click="issuePage--">上一页</button>
            <span role="status">第 {{ issuePage + 1 }} / {{ pageCount(issues.total) }} 页</span>
            <button type="button" class="secondary" :disabled="busy || issuePage + 1 >= pageCount(issues.total)" @click="issuePage++">下一页</button>
          </nav>
        </template>
        <p v-else-if="issuesOpen" role="status">正在加载问题明细…</p>
      </details>

      <p v-if="!summary.checked">
        {{ summary.found ? "所发现的照片均已跳过。" : summary.skipped ? "没有可检查的照片，请查看问题明细。" : "目录中未发现照片。" }}
      </p>
      <p v-else-if="!summary.groups">{{ outcomes.length ? "当前结果中已无可列出的相似组，保留照片仍在原目录中。" : "未发现相似照片组。" }}</p>
      <template v-else>
        <div class="result-filter">
          <label for="similar-filter">匹配类型</label>
          <select id="similar-filter" v-model="filter" :disabled="busy">
            <option value="all">全部</option>
            <option value="variant">同图变体</option>
            <option value="burst">疑似连拍</option>
          </select>
        </div>
        <p v-if="groupError" class="error" role="alert">{{ groupError }}</p>
        <button v-if="groupError" type="button" class="secondary" :disabled="busy" @click="revision++">重新加载相似组</button>
        <p v-else-if="!groups" role="status">正在加载相似组…</p>
        <p v-else-if="!groups.total" role="status">当前筛选条件下没有相似组。</p>
        <template v-else>
          <article v-for="group in groups.items" :key="group.id" class="similar-group">
            <div class="group-header">
              <SimilarPhotoPreview :scan-id="summary.scanId" :file-id="group.representative.id" :name="group.representative.name" />
              <div class="photo-info">
                <h3>{{ group.representative.name }}</h3>
                <p>代表图 · {{ group.count }} 张<span v-if="group.hasVariant"> · 同图变体</span><span v-if="group.hasBurst"> · 疑似连拍</span></p>
                <button type="button" class="secondary" :disabled="busy" :aria-expanded="expanded?.id === group.id"
                  @click="toggleGroup(group)">
                  {{ expanded?.id === group.id ? "收起对比" : "展开对比" }}
                </button>
              </div>
            </div>

            <div v-if="expanded?.id === group.id" class="group-content">
              <section id="similar-comparison" tabindex="-1" aria-labelledby="similar-compare-heading">
                <div class="compare-header">
                  <h4 id="similar-compare-heading">照片对比</h4>
                  <p class="selection-total" role="status">已对比 {{ compared.length }} / {{ compareLimit }} 张</p>
                </div>
                <p v-if="singleTrashError" class="error" role="alert">{{ singleTrashError }}</p>
                <ul v-if="compared.length" class="compare" :class="{ 'single-card': compared.length === 1 }">
                  <li v-for="member in compared" :key="member.photo.id" class="compare-card photo-info">
                    <div class="compare-preview">
                      <SimilarPhotoPreview :scan-id="summary.scanId" :file-id="member.photo.id" :name="member.photo.name" detail />
                      <button type="button" class="compare-open" aria-haspopup="dialog" :disabled="busy"
                        :aria-label="`放大查看 ${member.photo.relativePath}`" @click="openPhotoDetails(member, $event)">
                        <span>放大查看</span>
                      </button>
                    </div>
                    <div class="compare-meta">
                      <h5 :title="member.photo.relativePath">{{ member.photo.name }}</h5>
                      <p>{{ member.kind ? labels[member.kind] : "代表图" }} · {{ member.photo.width }} × {{ member.photo.height }} · {{ formatBytes(member.photo.size) }}</p>
                      <p v-if="member.photo.capturedAt">拍摄于 {{ member.photo.capturedAt }}</p>
                    </div>
                    <div class="compare-actions">
                      <button type="button" class="secondary" :disabled="busy"
                        :aria-label="`移出对比 ${member.photo.relativePath}`" @click="toggleComparison(member, true)">
                        移出对比
                      </button>
                      <span v-if="member.photo.hardLink" class="selection-total">硬链接，保留</span>
                      <button v-else type="button" class="secondary compare-trash" :disabled="!canTrashOne(member.photo)"
                        :aria-label="`移到回收站 ${member.photo.relativePath}`" @click="openTrashConfirmation(member.photo)">
                        移到回收站
                      </button>
                    </div>
                  </li>
                </ul>
                <p v-else-if="members" class="selection-total">点击下方缩略图，选择要对比的照片。</p>
              </section>

              <div class="actions">
                <button type="button" class="secondary" :disabled="busy || !selected.size" @click="selected = new Map()">清除勾选</button>
                <button type="button" class="danger" :disabled="busy || !selected.size" @click="openTrashConfirmation()">将勾选项移到回收站</button>
              </div>
              <p class="selection-total" role="status">已勾选 {{ selected.size }} 张，共 {{ formatBytes(selectedBytes) }}；本组保留 {{ group.count - selected.size }} 张。</p>
              <p id="similar-selection-rule">请手动勾选要移到回收站的照片，每组至少保留一张。代表图也可勾选，硬链接照片保留。</p>
              <p v-if="memberError" class="error" role="alert">{{ memberError }}</p>
              <button v-if="memberError" type="button" class="secondary" :disabled="busy" @click="expanded = { ...group }">重新加载组内照片</button>
              <template v-else-if="members">
                <p v-if="compared.length === compareLimit && group.count > compareLimit" class="selection-total">
                  点击已选缩略图或「移出对比」可腾出位置，再点击另一张缩略图加入对比。
                </p>
                <ul class="member-list">
                  <li v-for="{ photo, kind } in members.items" :key="photo.id">
                    <label class="photo-selection">
                      <input type="checkbox" :checked="selected.has(photo.id)" :disabled="selectionDisabled(photo)"
                        :aria-label="`勾选 ${photo.relativePath} 移到回收站`" aria-describedby="similar-selection-rule"
                        @change="toggleSelection(photo)" />
                    </label>
                    <div class="member-thumb">
                      <SimilarPhotoPreview :scan-id="summary.scanId" :file-id="photo.id" :name="photo.name" aria-hidden="true" />
                      <button type="button" class="compare-thumb" :aria-label="`对比 ${photo.relativePath}`"
                        :aria-pressed="comparedIds.has(photo.id)" :disabled="comparisonDisabled(photo)"
                        @click="toggleComparison({ photo, kind })"></button>
                    </div>
                    <div class="photo-info">
                      <strong>{{ photo.name }}</strong><span v-if="comparedIds.has(photo.id)" class="compare-tag">对比中</span>
                      <p>{{ photo.relativePath }}</p>
                      <p>{{ kind ? labels[kind] : "代表图" }} · {{ photo.width }} × {{ photo.height }} · {{ formatBytes(photo.size) }}</p>
                      <p v-if="photo.capturedAt">拍摄于 {{ photo.capturedAt }}</p>
                      <p v-if="photo.hardLink">硬链接，保留</p>
                      <p v-else-if="!selected.has(photo.id) && selected.size === group.count - 1">本组保留项</p>
                    </div>
                  </li>
                </ul>
              </template>
              <p v-else role="status">正在加载组内照片…</p>
              <nav v-if="pageCount(group.count) > 1" class="actions" aria-label="组内照片分页">
                <button type="button" class="secondary" :disabled="busy || memberPage === 0" @click="memberPage--">上一页</button>
                <span role="status">第 {{ memberPage + 1 }} / {{ pageCount(group.count) }} 页</span>
                <button type="button" class="secondary" :disabled="busy || memberPage + 1 >= pageCount(group.count)" @click="memberPage++">下一页</button>
              </nav>
            </div>
          </article>
          <nav v-if="pageCount(groups.total) > 1" class="actions" aria-label="相似组分页">
            <button type="button" class="secondary" :disabled="busy || groupPage === 0" @click="changeGroupPage(groupPage - 1)">上一页</button>
            <span role="status">第 {{ groupPage + 1 }} / {{ pageCount(groups.total) }} 页</span>
            <button type="button" class="secondary" :disabled="busy || groupPage + 1 >= pageCount(groups.total)" @click="changeGroupPage(groupPage + 1)">下一页</button>
          </nav>
        </template>
      </template>
    </section>

    <SimilarPhotoDetails v-if="viewing && summary" :key="viewing.member.photo.id"
      :scan-id="summary.scanId" :photo="viewing.member.photo"
      :match-label="viewing.member.kind ? labels[viewing.member.kind] : '代表图'" @close="closePhotoDetails" />

    <dialog ref="confirmation" class="trash-dialog" aria-labelledby="similar-trash-heading" aria-describedby="similar-trash-summary" @close="clearTrashRequest">
      <h2 id="similar-trash-heading">{{ trashRequest?.mode === "single" ? "将这张照片移到回收站？" : "确认移到回收站" }}</h2>
      <p id="similar-trash-summary">
        将移入 {{ trashRequest?.photos.length ?? 0 }} 张照片，逻辑大小共 {{ formatBytes(confirmationBytes) }}；本组保留 {{ (expanded?.count ?? 0) - (trashRequest?.photos.length ?? 0) }} 张。可从系统回收站恢复。
      </p>
      <p class="current-file">扫描目录 {{ directory }}</p>
      <div class="trash-preview" tabindex="0" aria-label="将移到回收站的照片">
        <ul><li v-for="photo in trashRequest?.photos" :key="photo.id">
          <strong v-if="trashRequest?.mode === 'single'">{{ photo.name }}</strong>
          <span v-if="trashRequest?.mode !== 'single' || photo.relativePath !== photo.name">{{ photo.relativePath }}</span>
        </li></ul>
      </div>
      <div class="actions">
        <button type="button" class="secondary" autofocus @click="confirmation?.close()">取消</button>
        <button type="button" class="danger" :disabled="busy || !trashRequest" @click="trashPhotos">确认移到回收站</button>
      </div>
    </dialog>
  </div>
</template>

<style scoped>
.result-filter,
.group-header,
.member-list li {
  display: flex;
  gap: 0.75rem;
  align-items: center;
}

.result-filter {
  margin: 1rem 0;
}

.result-filter label {
  margin: 0;
}

select {
  padding: 0.45rem;
  font: inherit;
  color: inherit;
}

.similar-group {
  border: 1px solid var(--border);
  border-radius: 0.65rem;
  margin-bottom: 0.75rem;
  padding: 0.75rem;
}

.group-header > :first-child {
  flex: none;
  width: clamp(5rem, 20vw, 8rem);
}

.photo-info {
  min-width: 0;
  overflow-wrap: anywhere;
}

.photo-info h3 {
  margin: 0 0 0.4rem;
}

.photo-info p {
  margin: 0.25rem 0 0.5rem;
  font-size: 0.875rem;
}

.group-content {
  border-top: 1px solid var(--border);
  margin-top: 0.75rem;
  padding-top: 0.75rem;
}

#similar-comparison {
  container-type: inline-size;
}

.compare-header {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 0.25rem 0.75rem;
  margin-bottom: 0.65rem;
}

.compare-header > * {
  margin: 0;
}

.compare {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0.75rem;
  margin: 0.75rem 0;
  padding: 0;
  list-style: none;
}

.compare.single-card {
  grid-template-columns: minmax(0, 1fr);
}

@container (max-width: 22rem) {
  .compare {
    grid-template-columns: minmax(0, 1fr);
  }
}

.compare-card {
  display: flex;
  flex-direction: column;
  gap: 0.65rem;
  border: 1px solid var(--border);
  border-radius: 0.65rem;
  padding: 0.65rem;
  background: var(--surface-subtle);
}

.compare-preview {
  position: relative;
}

.compare-preview > .photo-preview {
  height: clamp(8rem, 25dvh, 15rem);
  aspect-ratio: auto;
}

button.compare-open {
  position: absolute;
  inset: 0;
  display: flex;
  align-items: end;
  justify-content: end;
  width: 100%;
  padding: 0.5rem;
  cursor: zoom-in;
}

button.compare-open,
button.compare-open:hover:not(:disabled),
button.compare-open:disabled {
  background: transparent;
}

.compare-open span {
  padding: 0.25rem 0.5rem;
  border-radius: 0.35rem;
  background: var(--viewer);
  color: var(--viewer-text);
  font-size: 0.75rem;
}

.compare-open:disabled span {
  opacity: 0.6;
}

.compare-meta h5 {
  margin: 0;
  font-size: 0.95rem;
}

.compare-meta p {
  margin: 0.25rem 0 0;
}

.compare-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.4rem;
  margin-top: auto;
}

.compare-actions button {
  padding-inline: 0.6rem;
}

button.compare-trash:not(:disabled) {
  margin-inline-start: auto;
  color: var(--danger);
}

button.compare-trash:hover:not(:disabled) {
  border-color: var(--danger);
  background: var(--danger-soft);
}

.member-list {
  margin: 0;
  padding: 0;
  list-style: none;
}

.member-list li {
  border-top: 1px solid var(--border);
  padding: 0.65rem 0;
}

.member-thumb {
  position: relative;
  width: 4rem;
  flex: none;
}

button.compare-thumb {
  position: absolute;
  inset: 0;
  width: 100%;
  padding: 0;
  border: 2px solid transparent;
  background: transparent;
}

button.compare-thumb:hover:not(:disabled),
button.compare-thumb[aria-pressed="true"],
button.compare-thumb[aria-pressed="true"]:disabled {
  border-color: var(--accent);
  background: transparent;
}

button.compare-thumb:disabled {
  background: transparent;
}

.compare-tag {
  margin-inline-start: 0.4rem;
  color: var(--accent);
  font-size: 0.75rem;
}

.photo-selection {
  margin: 0;
  flex: none;
  display: flex;
  padding: 0.5rem 0;
}

.photo-selection input {
  width: 1rem;
  height: 1rem;
  margin: 0;
}

.selection-total {
  color: var(--muted);
}

.batch-result {
  border-block: 1px solid var(--border);
  margin-block: 1rem;
  padding-bottom: 0.75rem;
}

.duplicate-list li > :is(strong, span) {
  display: block;
  overflow-wrap: anywhere;
}

.trash-dialog {
  box-sizing: border-box;
  width: min(36rem, calc(100% - 2rem));
}

.trash-preview {
  max-height: 35vh;
  overflow: auto;
  overflow-wrap: anywhere;
  border-block: 1px solid var(--border);
}

.trash-preview li > strong {
  display: block;
}

.trash-dialog .actions {
  justify-content: end;
}

</style>
