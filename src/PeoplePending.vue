<script setup lang="ts">
import { computed, ref, shallowRef, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import PeopleFaceEditor from "./PeopleFaceEditor.vue";
import type { FaceEditKind, PendingGroupResume, PendingOverview } from "./people";

const props = defineProps<{ scanId: number; revision: number; writable: boolean; busy: boolean }>();
const emit = defineEmits<{ changed: []; preview: [fileId: number] }>();
const overview = shallowRef<PendingOverview>();
const active = shallowRef<number | "ungrouped" | "dissolved">();
const resumeGroup = shallowRef<PendingGroupResume>();
const snapshotRevision = ref(props.revision);
const page = ref(0);
const refresh = ref(0);
const loading = ref(false);
const error = ref("");
const stale = ref(false);
let keepAfterIgnore = false;

const view = computed(() => active.value !== undefined && active.value !== "dissolved" && overview.value
  ? { token: overview.value.token, groupId: active.value === "ungrouped" ? null : active.value }
  : undefined);

function closeDetail() {
  active.value = undefined;
  resumeGroup.value = undefined;
  keepAfterIgnore = false;
}

watch(() => props.scanId, () => {
  closeDetail();
  page.value = 0;
  stale.value = false;
});
watch(() => props.revision, () => {
  if (keepAfterIgnore) {
    keepAfterIgnore = false;
    return;
  }
  if (active.value !== undefined) stale.value = true;
  closeDetail();
  page.value = 0;
});
watch([() => props.scanId, () => props.revision, page, refresh], async (_, __, cleanup) => {
  let current = true;
  cleanup(() => { current = false; });
  loading.value = true;
  error.value = "";
  if (active.value === undefined) overview.value = undefined;
  const requestedResume = resumeGroup.value;
  const revision = props.revision;
  try {
    const result = await invoke<PendingOverview>("people_pending_groups", {
      scanId: props.scanId, page: page.value, resumeGroup: requestedResume,
    });
    if (!current) return;
    if (requestedResume && resumeGroup.value === requestedResume) {
      active.value = result.resumedGroupId ?? "dissolved";
      resumeGroup.value = undefined;
    }
    overview.value = result;
    snapshotRevision.value = revision;
  } catch (cause) {
    if (current) error.value = String(cause);
  } finally {
    if (current) loading.value = false;
  }
}, { immediate: true });

function reload() {
  refresh.value++;
}

function onStale() {
  error.value = "待整理分组已变化，请重新读取。";
}

function onChanged(kind: FaceEditKind, remainingFaceIds?: number[]) {
  if (kind === "ignore") {
    keepAfterIgnore = true;
    if (typeof active.value === "number" && remainingFaceIds) {
      resumeGroup.value = { groupId: active.value, faceIds: remainingFaceIds };
    }
  } else {
    closeDetail();
  }
  stale.value = false;
  emit("changed");
}

function open(group: number | "ungrouped") {
  stale.value = false;
  active.value = group;
}
</script>

<template>
  <section class="pending-panel" aria-label="待整理人脸" :aria-busy="loading">
    <p v-if="stale" role="status">分组已更新，请重新选择人脸。</p>
    <p v-if="loading" role="status">正在读取待整理分组…</p>
    <div v-if="error" class="pending-actions">
      <p class="error" role="alert">{{ error }}</p>
      <button type="button" class="secondary" @click="reload">重新读取</button>
    </div>
    <template v-if="active !== undefined">
      <div class="pending-actions">
        <button type="button" class="secondary" @click="closeDetail">返回待整理</button>
        <h2>{{ active === 'ungrouped' ? '未成组的人脸' : '相似人脸组' }}</h2>
      </div>
      <PeopleFaceEditor v-if="view" v-show="!loading && !error" :scan-id="scanId" scope="pending" :pending-view="view"
        :revision="snapshotRevision" :writable="writable" :busy="busy || loading || !!error"
        @changed="onChanged" @preview="emit('preview', $event)" @stale="onStale" />
      <p v-else-if="active === 'dissolved' && !loading && !error" class="muted">这个相似人脸组已解散。</p>
    </template>
    <template v-else-if="overview && !loading && !error">
      <div v-if="overview.ungroupedCount" class="pending-actions">
        <button type="button" class="secondary" @click="open('ungrouped')">未成组的人脸（{{ overview.ungroupedCount }}）</button>
      </div>
      <template v-if="overview.groups.total">
        <h2>相似人脸组</h2>
        <ul class="pending-groups">
          <li v-for="group in overview.groups.items" :key="group.id">
            <button type="button" class="secondary group-card" @click="open(group.id)">
              <img :src="group.thumbnail" alt="" width="96" height="96" loading="lazy" />
              <span>{{ group.count }} 张人脸</span>
            </button>
          </li>
        </ul>
        <nav v-if="overview.groups.total > 24" class="pending-actions" aria-label="待整理分组分页">
          <button type="button" class="secondary" :disabled="overview.groups.page === 0" @click="page = overview.groups.page - 1">上一页</button>
          <span>{{ overview.groups.page + 1 }} / {{ Math.ceil(overview.groups.total / 24) }}</span>
          <button type="button" class="secondary" :disabled="(overview.groups.page + 1) * 24 >= overview.groups.total" @click="page = overview.groups.page + 1">下一页</button>
        </nav>
      </template>
      <p v-else-if="!overview.ungroupedCount">这里暂时没有待整理的人脸。</p>
    </template>
  </section>
</template>

<style scoped>
.pending-panel { margin: 1rem 0; }
.pending-panel h2 { margin: .5rem 0; font-size: 1.1rem; }
.pending-actions { display: flex; align-items: center; flex-wrap: wrap; gap: .75rem; margin: .75rem 0; }
.pending-groups { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 145px), 1fr)); gap: .75rem; list-style: none; padding: 0; }
.group-card { width: 100%; display: flex; flex-direction: column; align-items: center; gap: .5rem; padding: .75rem; }
.group-card img { border-radius: .5rem; object-fit: cover; }
</style>
