<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, onMounted, ref, shallowRef } from "vue";
import { ElButton, ElInput, ElSelect, ElOption } from "element-plus";
import { formatBytes } from "./formatBytes";
import type { MapPackage, MapState, Region } from "./locations";

const props = defineProps<{ bounds: [number, number, number, number] | null }>();
const emit = defineEmits<{ changed: [packages: MapPackage[]] }>();
const state = shallowRef<MapState>();
const regions = shallowRef<Region[]>([]);
const selectedRegion = ref("");
const recoveredBounds = shallowRef<[number, number, number, number] | null>(null);
const regionQuery = ref("");
const name = ref("");
const busy = ref(false);
const error = ref("");
const progress = ref("");
const bytes = ref(0);
const visibleRegions = computed(() => {
  const query = regionQuery.value.trim();
  const selected = regions.value.find(r => r.id === selectedRegion.value);
  const result = regions.value.filter(r => query ? r.name.includes(query) : r.level <= 4).slice(0, 100);
  if (selected && !result.some(r => r.id === selected.id)) result.unshift(selected);
  return result;
});
const region = computed(() => regions.value.find(r => r.id === selectedRegion.value));
const downloadBounds = computed(() => region.value?.bounds ?? props.bounds ?? recoveredBounds.value);
async function refresh() {
  state.value = await invoke<MapState>("offline_map_state");
  if (state.value.interruptedDownload && !recoveredBounds.value) {
    recoveredBounds.value = state.value.interruptedDownload.bounds;
    if (!name.value) name.value = state.value.interruptedDownload.name;
  }
  emit("changed", state.value.packages);
}
onMounted(async () => {
  try { await Promise.all([refresh(), invoke<Region[]>("location_regions").then(v => { regions.value = v; })]); }
  catch (cause) { error.value = String(cause); }
});
function channel() {
  return new Channel<{ phase: string; bytes: number }>(update => { progress.value = update.phase; bytes.value = update.bytes; });
}
async function task(action: () => Promise<unknown>) {
  if (busy.value) return;
  busy.value = true; error.value = ""; progress.value = "正在准备…"; bytes.value = 0;
  try { await action(); await refresh(); }
  catch (cause) { error.value = String(cause); try { await refresh(); } catch { /* Keep the original error. */ } }
  finally { busy.value = false; progress.value = ""; }
}
function download() {
  const bounds = downloadBounds.value;
  if (!bounds) return;
  void task(() => invoke("offline_map_download", { bounds, name: name.value.trim() || region.value?.name || "框选区域", progress: channel() }));
}
function importPackage() { void task(() => invoke("offline_map_import", { progress: channel() })); }
function chooseDirectory() { void task(() => invoke("offline_map_choose_directory")); }
function remove(id: string) { void task(() => invoke("offline_map_delete", { id })); }
async function cancel() { try { await invoke("offline_map_cancel"); } catch (cause) { error.value = String(cause); } }
</script>

<template>
  <details class="offline-settings">
    <summary>离线地图包（{{ state?.packages.length ?? 0 }}）</summary>
    <p>先联网下载需要的区域，之后可断网浏览街道。也可导入 Protomaps v4 格式的 PMTiles 地图包。</p>
    <p class="map-directory">保存目录：{{ state?.directory || '正在读取…' }}</p>
    <div class="map-package-actions">
      <ElButton :disabled="busy" @click="chooseDirectory">更换保存目录</ElButton>
      <ElButton :disabled="busy" @click="importPackage">导入地图包</ElButton>
    </div>
    <p v-if="state?.interrupted" role="status">上次下载未完成，已有地图仍可使用。重新选择区域后可重试下载。</p>
    <div class="download-region">
      <ElSelect v-model="selectedRegion" filterable remote clearable :remote-method="(q: string) => regionQuery = q" :disabled="busy"
        aria-label="选择下载行政区" placeholder="搜索省市区，或使用地图框选区域">
        <ElOption v-for="entry in visibleRegions" :key="entry.id" :value="entry.id" :label="entry.name" />
      </ElSelect>
      <ElInput v-model="name" :disabled="busy" aria-label="地图包名称" placeholder="地图包名称（可选）" maxlength="120" />
      <ElButton type="primary" :disabled="busy || !downloadBounds" @click="download">{{ error ? '重试下载区域' : '下载区域地图' }}</ElButton>
    </div>
    <p v-if="downloadBounds" class="map-bounds">下载范围：{{ downloadBounds.map(v => v.toFixed(3)).join(', ') }} · 街道细节 z15</p>
    <p v-else>可先在地图上点击“框选”指定下载范围，也可搜索行政区。</p>
    <p v-if="busy" role="status">{{ progress }}<span v-if="bytes"> · 已写入 {{ formatBytes(bytes) }}</span> <ElButton @click="cancel">取消</ElButton></p>
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <ul v-if="state?.packages.length" class="map-packages">
      <li v-for="entry in state.packages" :key="entry.id">
        <div><strong>{{ entry.name }}</strong><p v-if="entry.warning" class="error">{{ entry.warning }}</p><span>{{ formatBytes(entry.size) }} · z{{ entry.minZoom }}–{{ entry.maxZoom }} · {{ new Date(entry.installedAt).toLocaleDateString() }}</span></div>
        <ElButton :disabled="busy" @click="remove(entry.id)">删除</ElButton>
      </li>
    </ul>
    <p>更换目录不会移动已有地图。重叠区域优先显示最新安装的地图包。</p>
  </details>
</template>

<style scoped>
.offline-settings { border-top: 1px solid var(--border); padding-top: .75rem; margin-top: .75rem; }
summary { cursor: pointer; font-weight: 600; }
p { color: var(--muted); font-size: .875rem; }
.map-directory { overflow-wrap: anywhere; }
.map-package-actions,.download-region { display: flex; flex-wrap: wrap; gap: .6rem; align-items: center; }
.download-region { margin-top: .8rem; }
.download-region .el-select { width: min(100%, 25rem); }
.download-region .el-input { width: 14rem; }
.map-packages { list-style: none; padding: 0; }
.map-packages li { display: flex; justify-content: space-between; align-items: center; padding: .6rem 0; border-bottom: 1px solid var(--border); gap: 1rem; }
.map-packages span { display: block; margin-top: .25rem; color: var(--muted); font-size: .8rem; }
.error { color: var(--danger); }
</style>
