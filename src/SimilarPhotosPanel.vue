<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, nextTick, ref, shallowRef, watch } from "vue";
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

const pageSize = 25;
const labels = { variant: "同图变体", burst: "疑似连拍" };
const emit = defineEmits<{ busyChange: [busy: boolean] }>();
const directory = ref("");
const choosing = ref(false);
const activeScan = ref<number>();
const cancelling = ref(false);
const trashing = ref(false);
const busy = computed(() => choosing.value || activeScan.value !== undefined || trashing.value);
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
const left = shallowRef<Photo>();
const right = shallowRef<Photo>();
const selected = shallowRef(new Map<number, Photo>());
const selectedBytes = computed(() => [...selected.value.values()].reduce((total, photo) => total + photo.size, 0));
const confirmation = ref<HTMLDialogElement>();
const resultsSection = ref<HTMLElement>();
const outcomes = shallowRef<TrashOutcome[]>([]);
const trashError = ref("");
const revision = ref(0);
let refreshGroup: { preferredId?: number } | undefined;
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
  selected.value = new Map();
  confirmation.value?.close();
  expanded.value = undefined;
  members.value = undefined;
  memberPage.value = 0;
  left.value = undefined;
  right.value = undefined;
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
  if (expanded.value?.id === group.id) {
    closeGroup();
    return;
  }
  closeGroup();
  expanded.value = group;
  memberPage.value = 0;
  left.value = group.representative;
  right.value = undefined;
}

function selectPhoto(photo: Photo, side: "left" | "right") {
  if (busy.value) return;
  const target = side === "left" ? left : right;
  const other = side === "left" ? right : left;
  if (other.value?.id === photo.id) other.value = target.value;
  target.value = photo;
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

async function trashSelection() {
  const current = summary.value;
  const group = expanded.value;
  if (!current || !group || busy.value || !selected.value.size || !confirmation.value?.open) return;
  const fileIds = [...selected.value.keys()];
  confirmation.value.close();
  trashing.value = true;
  trashError.value = "";
  outcomes.value = [];
  try {
    const result = await invoke<TrashResult>("trash_similar", { scanId: current.scanId, groupId: group.id, fileIds });
    summary.value = { ...current, groups: result.totalGroups };
    outcomes.value = result.outcomes;
    closeGroup();
    refreshGroup = { preferredId: result.affectedGroups.find((group) =>
      filter.value === "all" || (filter.value === "variant" ? group.hasVariant : group.hasBurst),
    )?.id };
    revision.value++;
  } catch (cause) {
    trashError.value = String(cause);
  } finally {
    trashing.value = false;
    await nextTick();
    resultsSection.value?.focus({ preventScroll: true });
  }
}

function pageCount(total: number) {
  return Math.max(1, Math.ceil(total / pageSize));
}

function changeGroupPage(page: number) {
  if (busy.value) return;
  closeGroup();
  groupPage.value = page;
}

watch(filter, () => changeGroupPage(0), { flush: "sync" });
watch([scanId, filter, groupPage, revision], async ([id, kind, page], _, onCleanup) => {
  groups.value = undefined;
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
      if (refreshGroup) {
        const group = result.items.find((group) => group.id === refreshGroup?.preferredId) ?? result.items[0];
        refreshGroup = undefined;
        if (group) toggleGroup(group);
      }
    }
  } catch (cause) {
    if (current) groupError.value = String(cause);
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
      if (!right.value && page === 0) right.value = result.items[1]?.photo;
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

    <section v-if="summary" ref="resultsSection" tabindex="-1" aria-labelledby="similar-results" :aria-busy="trashing">
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
              <div class="actions">
                <button type="button" class="secondary" :disabled="busy || !selected.size" @click="selected = new Map()">清除勾选</button>
                <button type="button" class="danger" :disabled="busy || !selected.size" @click="confirmation?.showModal()">移到回收站</button>
              </div>
              <p class="selection-total" role="status">已勾选 {{ selected.size }} 张，共 {{ formatBytes(selectedBytes) }}；本组保留 {{ group.count - selected.size }} 张。</p>
              <p id="similar-selection-rule">请手动勾选要移到回收站的照片，每组至少保留一张。代表图也可勾选，硬链接照片保留。</p>
              <div class="compare" aria-label="并排对比">
                <div v-for="(photo, side) in { left, right }" :key="side" class="photo-info">
                  <h4>{{ side === "left" ? "左图" : "右图" }}</h4>
                  <template v-if="photo">
                    <SimilarPhotoPreview :scan-id="summary.scanId" :file-id="photo.id" :name="photo.name" detail />
                    <p><strong>{{ photo.name }}</strong></p>
                    <p>{{ photo.relativePath }}</p>
                    <p>{{ photo.width }} × {{ photo.height }} · {{ formatBytes(photo.size) }}</p>
                    <p v-if="photo.capturedAt">拍摄于 {{ photo.capturedAt }}</p>
                  </template>
                </div>
              </div>
              <p v-if="memberError" class="error" role="alert">{{ memberError }}</p>
              <button v-if="memberError" type="button" class="secondary" :disabled="busy" @click="expanded = { ...group }">重新加载组内照片</button>
              <ul v-else-if="members" class="member-list">
                <li v-for="{ photo, kind } in members.items" :key="photo.id">
                  <label class="photo-selection">
                    <input type="checkbox" :checked="selected.has(photo.id)" :disabled="selectionDisabled(photo)"
                      :aria-label="`勾选 ${photo.relativePath} 移到回收站`" aria-describedby="similar-selection-rule"
                      @change="toggleSelection(photo)" />
                  </label>
                  <SimilarPhotoPreview :scan-id="summary.scanId" :file-id="photo.id" :name="photo.name" />
                  <div class="photo-info">
                    <strong>{{ photo.name }}</strong>
                    <p>{{ photo.relativePath }}</p>
                    <p>{{ kind ? labels[kind] : "代表图" }} · {{ photo.width }} × {{ photo.height }} · {{ formatBytes(photo.size) }}</p>
                    <p v-if="photo.capturedAt">拍摄于 {{ photo.capturedAt }}</p>
                    <p v-if="photo.hardLink">硬链接，保留</p>
                    <p v-else-if="!selected.has(photo.id) && selected.size === group.count - 1">本组保留项</p>
                    <div class="actions">
                      <button type="button" class="secondary" :aria-label="`${photo.name} 设为左图`"
                        :disabled="busy || left?.id === photo.id" @click="selectPhoto(photo, 'left')">
                        {{ left?.id === photo.id ? "当前左图" : "设为左图" }}
                      </button>
                      <button type="button" class="secondary" :aria-label="`${photo.name} 设为右图`"
                        :disabled="busy || right?.id === photo.id" @click="selectPhoto(photo, 'right')">
                        {{ right?.id === photo.id ? "当前右图" : "设为右图" }}
                      </button>
                    </div>
                  </div>
                </li>
              </ul>
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

    <dialog ref="confirmation" class="trash-dialog" aria-labelledby="similar-trash-heading" aria-describedby="similar-trash-summary">
      <h2 id="similar-trash-heading">确认移到回收站</h2>
      <p id="similar-trash-summary">
        将移入 {{ selected.size }} 张照片，逻辑大小共 {{ formatBytes(selectedBytes) }}；本组保留 {{ (expanded?.count ?? 0) - selected.size }} 张。可从系统回收站恢复。
      </p>
      <p class="current-file">扫描目录 {{ directory }}</p>
      <div class="trash-preview" tabindex="0" aria-label="将移到回收站的照片">
        <ul><li v-for="photo in selected.values()" :key="photo.id">{{ photo.relativePath }}</li></ul>
      </div>
      <div class="actions">
        <button type="button" class="secondary" autofocus @click="confirmation?.close()">取消</button>
        <button type="button" class="danger" :disabled="busy || !selected.size" @click="trashSelection">确认移到回收站</button>
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

.photo-info h3,
.photo-info h4 {
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

.compare {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0.75rem;
  margin-bottom: 0.75rem;
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

.member-list li > .photo-preview {
  width: 4rem;
  flex: none;
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

.trash-dialog .actions {
  justify-content: end;
}

.member-list .actions {
  margin: 0;
}
</style>
