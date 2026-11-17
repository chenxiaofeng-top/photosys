<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, ref, shallowRef, watch } from "vue";
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

const pageSize = 25;
const labels = { variant: "同图变体", burst: "疑似连拍" };
const emit = defineEmits<{ busyChange: [busy: boolean] }>();
const directory = ref("");
const choosing = ref(false);
const activeScan = ref<number>();
const cancelling = ref(false);
const busy = computed(() => choosing.value || activeScan.value !== undefined);
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

const issuesOpen = ref(false);
const issuePage = ref(0);
const issues = shallowRef<Page<Issue>>();
const issueError = ref("");

function closeGroup() {
  expanded.value = undefined;
  memberPage.value = 0;
  left.value = undefined;
  right.value = undefined;
}

function resetResults() {
  summary.value = undefined;
  filter.value = "all";
  groupPage.value = 0;
  issuesOpen.value = false;
  issuePage.value = 0;
  closeGroup();
}

async function chooseDirectory() {
  if (busy.value) return;
  choosing.value = true;
  scanError.value = "";
  try {
    const folder = await invoke<string | null>("choose_folder", { title: "选择相似照片扫描目录" });
    if (folder !== null && directory.value !== folder) {
      directory.value = folder;
      notice.value = "";
      resetResults();
    }
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
  if (expanded.value?.id === group.id) {
    closeGroup();
    return;
  }
  expanded.value = group;
  memberPage.value = 0;
  left.value = group.representative;
  right.value = undefined;
}

function selectPhoto(photo: Photo, side: "left" | "right") {
  const target = side === "left" ? left : right;
  const other = side === "left" ? right : left;
  if (other.value?.id === photo.id) other.value = target.value;
  target.value = photo;
}

function pageCount(total: number) {
  return Math.max(1, Math.ceil(total / pageSize));
}

function changeGroupPage(page: number) {
  closeGroup();
  groupPage.value = page;
}

watch(filter, () => changeGroupPage(0), { flush: "sync" });
watch([scanId, filter, groupPage], async ([id, kind, page], _, onCleanup) => {
  groups.value = undefined;
  groupError.value = "";
  if (id === undefined) return;
  let current = true;
  onCleanup(() => { current = false; });
  try {
    const result = await invoke<Page<Group>>("similar_groups", { scanId: id, kind: kind === "all" ? null : kind, page });
    if (current) groups.value = result;
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
    </form>

    <section v-if="summary" aria-labelledby="similar-results">
      <h2 id="similar-results">扫描结果</h2>
      <dl aria-live="polite">
        <div><dt>成功检查</dt><dd>{{ summary.checked }}</dd></div>
        <div><dt>复用缓存</dt><dd>{{ summary.reused }}</dd></div>
        <div><dt>跳过项</dt><dd>{{ summary.skipped }}</dd></div>
        <div><dt>相似组</dt><dd>{{ summary.groups }}</dd></div>
      </dl>
      <p v-if="summary.cacheWarning" role="status">{{ summary.cacheWarning }}</p>
      <p v-if="summary.unsupported">跳过了 {{ summary.unsupported }} 张格式暂不支持的照片。</p>
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
            <button type="button" class="secondary" :disabled="issuePage === 0" @click="issuePage--">上一页</button>
            <span role="status">第 {{ issuePage + 1 }} / {{ pageCount(issues.total) }} 页</span>
            <button type="button" class="secondary" :disabled="issuePage + 1 >= pageCount(issues.total)" @click="issuePage++">下一页</button>
          </nav>
        </template>
        <p v-else-if="issuesOpen" role="status">正在加载问题明细…</p>
      </details>

      <p v-if="!summary.checked">
        {{ summary.found ? "所发现的照片均已跳过。" : summary.skipped ? "没有可检查的照片，请查看问题明细。" : "目录中未发现照片。" }}
      </p>
      <p v-else-if="!summary.groups">未发现相似照片组。</p>
      <template v-else>
        <div class="result-filter">
          <label for="similar-filter">匹配类型</label>
          <select id="similar-filter" v-model="filter">
            <option value="all">全部</option>
            <option value="variant">同图变体</option>
            <option value="burst">疑似连拍</option>
          </select>
        </div>
        <p v-if="groupError" class="error" role="alert">{{ groupError }}</p>
        <p v-else-if="!groups" role="status">正在加载相似组…</p>
        <p v-else-if="!groups.total" role="status">当前筛选条件下没有相似组。</p>
        <template v-else>
          <article v-for="group in groups.items" :key="group.id" class="similar-group">
            <div class="group-header">
              <SimilarPhotoPreview :scan-id="summary.scanId" :file-id="group.representative.id" :name="group.representative.name" />
              <div class="photo-info">
                <h3>{{ group.representative.name }}</h3>
                <p>代表图 · {{ group.count }} 张<span v-if="group.hasVariant"> · 同图变体</span><span v-if="group.hasBurst"> · 疑似连拍</span></p>
                <button type="button" class="secondary" :aria-expanded="expanded?.id === group.id"
                  @click="toggleGroup(group)">
                  {{ expanded?.id === group.id ? "收起对比" : "展开对比" }}
                </button>
              </div>
            </div>

            <div v-if="expanded?.id === group.id" class="group-content">
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
              <ul v-else-if="members" class="member-list">
                <li v-for="{ photo, kind } in members.items" :key="photo.id">
                  <SimilarPhotoPreview :scan-id="summary.scanId" :file-id="photo.id" :name="photo.name" />
                  <div class="photo-info">
                    <strong>{{ photo.name }}</strong>
                    <p>{{ photo.relativePath }}</p>
                    <p>{{ kind ? labels[kind] : "代表图" }} · {{ photo.width }} × {{ photo.height }} · {{ formatBytes(photo.size) }}</p>
                    <p v-if="photo.capturedAt">拍摄于 {{ photo.capturedAt }}</p>
                    <div class="actions">
                      <button type="button" class="secondary" :aria-label="`${photo.name} 设为左图`"
                        :disabled="left?.id === photo.id" @click="selectPhoto(photo, 'left')">
                        {{ left?.id === photo.id ? "当前左图" : "设为左图" }}
                      </button>
                      <button type="button" class="secondary" :aria-label="`${photo.name} 设为右图`"
                        :disabled="right?.id === photo.id" @click="selectPhoto(photo, 'right')">
                        {{ right?.id === photo.id ? "当前右图" : "设为右图" }}
                      </button>
                    </div>
                  </div>
                </li>
              </ul>
              <p v-else role="status">正在加载组内照片…</p>
              <nav v-if="pageCount(group.count) > 1" class="actions" aria-label="组内照片分页">
                <button type="button" class="secondary" :disabled="memberPage === 0" @click="memberPage--">上一页</button>
                <span role="status">第 {{ memberPage + 1 }} / {{ pageCount(group.count) }} 页</span>
                <button type="button" class="secondary" :disabled="memberPage + 1 >= pageCount(group.count)" @click="memberPage++">下一页</button>
              </nav>
            </div>
          </article>
          <nav v-if="pageCount(groups.total) > 1" class="actions" aria-label="相似组分页">
            <button type="button" class="secondary" :disabled="groupPage === 0" @click="changeGroupPage(groupPage - 1)">上一页</button>
            <span role="status">第 {{ groupPage + 1 }} / {{ pageCount(groups.total) }} 页</span>
            <button type="button" class="secondary" :disabled="groupPage + 1 >= pageCount(groups.total)" @click="changeGroupPage(groupPage + 1)">下一页</button>
          </nav>
        </template>
      </template>
    </section>
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
  border: 1px solid #ceded8;
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
  border-top: 1px solid #ceded8;
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
  border-top: 1px solid #ceded8;
  padding: 0.65rem 0;
}

.member-list li > :first-child {
  width: 4rem;
  flex: none;
}

.member-list .actions {
  margin: 0;
}
</style>
