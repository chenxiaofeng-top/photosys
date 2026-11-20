<script setup lang="ts">
import { computed, ref, shallowRef, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import PeopleFaceEditor from "./PeopleFaceEditor.vue";
import type { PendingGroup, PendingOverview } from "./people";

const props = defineProps<{ scanId: number; revision: number; writable: boolean; busy: boolean }>();
const emit = defineEmits<{ changed: []; preview: [fileId: number] }>();
const overview = shallowRef<PendingOverview>();
const active = shallowRef<PendingGroup | "ungrouped">();
const page = ref(0);
const refresh = ref(0);
const loading = ref(false);
const error = ref("");
const stale = ref(false);

const view = computed(() => active.value && overview.value
  ? { token: overview.value.token, groupId: active.value === "ungrouped" ? null : active.value.id }
  : undefined);

watch(() => props.scanId, () => {
  active.value = undefined;
  page.value = 0;
  stale.value = false;
});
watch(() => props.revision, () => {
  if (active.value) stale.value = true;
  active.value = undefined;
  page.value = 0;
});
watch([() => props.scanId, () => props.revision, page, refresh], async (_, __, cleanup) => {
  let current = true;
  cleanup(() => { current = false; });
  loading.value = true;
  error.value = "";
  overview.value = undefined;
  try {
    const result = await invoke<PendingOverview>("people_pending_groups", { scanId: props.scanId, page: page.value });
    if (current) overview.value = result;
  } catch (cause) {
    if (current) error.value = String(cause);
  } finally {
    if (current) loading.value = false;
  }
}, { immediate: true });

function reload() {
  active.value = undefined;
  refresh.value++;
}

function onStale() {
  stale.value = true;
  reload();
}

function onChanged() {
  active.value = undefined;
  stale.value = false;
  emit("changed");
}

function open(group: PendingGroup | "ungrouped") {
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
    <template v-if="view">
      <div class="pending-actions">
        <button type="button" class="secondary" @click="active = undefined">返回待整理</button>
        <h2>{{ active === 'ungrouped' ? '未成组的人脸' : '相似人脸组' }}</h2>
      </div>
      <PeopleFaceEditor :scan-id="scanId" scope="pending" :pending-view="view" :revision="revision"
        :writable="writable" :busy="busy" @changed="onChanged" @preview="emit('preview', $event)" @stale="onStale" />
    </template>
    <template v-else-if="overview">
      <div v-if="overview.ungroupedCount" class="pending-actions">
        <button type="button" class="secondary" @click="open('ungrouped')">未成组的人脸（{{ overview.ungroupedCount }}）</button>
      </div>
      <template v-if="overview.groups.total">
        <h2>相似人脸组</h2>
        <ul class="pending-groups">
          <li v-for="group in overview.groups.items" :key="group.id">
            <button type="button" class="secondary group-card" @click="open(group)">
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
