<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { computed, nextTick, ref, shallowRef, watch } from "vue";
import { validAlbumName, type Album, type AlbumPage, type AlbumEdit, type AlbumEditResult } from "./albums";
const props = defineProps<{ scanId: number; fileIds: number[]; libraryBusy: boolean; revision: number }>();
const emit = defineEmits<{ added: [album: Album]; busyChange: [busy: boolean] }>();
const dialog = ref<HTMLDialogElement>();
const opened = ref(false);
const query = ref("");
const committedQuery = ref("");
const page = ref(0);
const result = shallowRef<AlbumPage>();
const target = shallowRef<Album>();
const mode = ref<"existing" | "new">("existing");
const name = ref("");
const loading = ref(false);
const saving = ref(false);
const error = ref("");
const retry = ref(0);
const disabled = computed(() => props.libraryBusy || saving.value || !result.value?.writable);
async function open(preset?: Album) {
  target.value = preset; mode.value = "existing"; name.value = ""; error.value = "";
  query.value = ""; committedQuery.value = ""; page.value = 0; result.value = undefined; opened.value = true;
  await nextTick(); dialog.value?.showModal();
}
function close() { if (saving.value) return; opened.value = false; dialog.value?.close(); }
watch(() => props.scanId, close);
watch(query, (_, __, cleanup) => {
  const timer = setTimeout(() => { committedQuery.value = query.value; page.value = 0; }, 250);
  cleanup(() => clearTimeout(timer));
});
watch([opened, () => props.scanId, committedQuery, page, () => props.revision, retry], async (_, __, cleanup) => {
  let current = true; cleanup(() => { current = false; });
  if (!opened.value) return;
  loading.value = true; error.value = "";
  try {
    const value = await invoke<AlbumPage>("albums_list", { scanId: props.scanId, query: committedQuery.value, page: page.value });
    if (current) result.value = value;
  } catch (cause) { if (current) { error.value = String(cause); result.value = undefined; } }
  finally { if (current) loading.value = false; }
});
async function save() {
  if (disabled.value || !props.fileIds.length || (mode.value === "new" ? !validAlbumName(name.value) : !target.value)) return;
  const scanId = props.scanId;
  const edit: AlbumEdit = mode.value === "new" ? { kind: "create", name: name.value, fileIds: [...props.fileIds] }
    : { kind: "add", albumId: target.value!.id, fileIds: [...props.fileIds] };
  saving.value = true; emit("busyChange", true); error.value = "";
  try {
    const value = await invoke<AlbumEditResult>("albums_edit", { scanId, edit });
    if (scanId !== props.scanId) return;
    emit("added", mode.value === "new" ? { id: value.albumId, name: name.value.trim(), count: props.fileIds.length, cover: null } : target.value!);
    opened.value = false; dialog.value?.close();
  } catch (cause) { if (scanId === props.scanId) error.value = String(cause); }
  finally { saving.value = false; emit("busyChange", false); }
}
defineExpose({ open, close });
</script>
<template>
  <dialog ref="dialog" class="album-picker" aria-labelledby="album-picker-title" @cancel.prevent="close" @close="opened = false">
    <h2 id="album-picker-title">将选中的 {{ fileIds.length }} 项加入相册</h2>
    <p>同一文件可加入多个相册，原文件保留在原目录。</p>
    <div class="picker-actions" role="group" aria-label="加入方式">
      <button type="button" :class="{ secondary: mode !== 'existing' }" :aria-pressed="mode === 'existing'" :disabled="saving" @click="mode = 'existing'">已有相册</button>
      <button type="button" :class="{ secondary: mode !== 'new' }" :aria-pressed="mode === 'new'" :disabled="saving" @click="mode = 'new'">新建相册</button>
    </div>
    <template v-if="mode === 'existing'">
      <label class="picker-search">查找相册 <input v-model="query" type="search" :disabled="saving" placeholder="输入活动或情景名称" /></label>
      <p v-if="target" role="status">已选：{{ target.name }}</p>
      <p v-if="loading" role="status">正在读取相册…</p>
      <ul v-else-if="result?.items.length" class="picker-list">
        <li v-for="album in result.items" :key="album.id"><label><input type="radio" name="album-target" :checked="target?.id === album.id" :disabled="disabled" @change="target = album" />{{ album.name }} <span class="status">{{ album.count }} 项</span></label></li>
      </ul>
      <p v-else-if="result">{{ committedQuery ? '没有找到匹配的相册。' : '还没有相册，可切换到“新建相册”。' }}</p>
      <nav v-if="result && result.total > 24" class="picker-actions" aria-label="选择相册分页">
        <button type="button" class="secondary" :disabled="saving || loading || result.page === 0" @click="page = result.page - 1">上一页</button>
        <span>{{ result.page + 1 }} / {{ Math.ceil(result.total / 24) }}</span>
        <button type="button" class="secondary" :disabled="saving || loading || (result.page + 1) * 24 >= result.total" @click="page = result.page + 1">下一页</button>
      </nav>
    </template>
    <form @submit.prevent="save">
      <label v-if="mode === 'new'" class="picker-search">相册名称 <input v-model="name" maxlength="160" placeholder="例如：杭州之旅" :disabled="disabled" required /></label>
      <p v-if="mode === 'new' && name.trim() && !validAlbumName(name)" class="error" role="alert">相册名称最多 80 个字符。</p>
      <p v-if="result && !result.writable" role="status">此媒体库只读，无法保存相册归类。</p>
      <p v-if="error" class="error" role="alert">{{ error }} <button type="button" class="secondary" :disabled="saving || loading" @click="retry++">重试读取</button></p>
      <div class="picker-actions">
        <button type="button" class="secondary" :disabled="saving" @click="close">取消</button>
        <button type="submit" :disabled="disabled || !fileIds.length || (mode === 'new' ? !validAlbumName(name) : !target)">{{ saving ? '正在保存…' : mode === 'new' ? '创建并加入' : '加入相册' }}</button>
      </div>
    </form>
  </dialog>
</template>
<style scoped>
.album-picker { width: min(560px, calc(100vw - 32px)); max-height: calc(100dvh - 48px); overflow-y: auto; }
.picker-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; margin-block: 16px; }
.picker-search { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-top: 16px; }
.picker-search input { min-width: 0; max-width: 100%; flex: 1; }
.picker-list { padding: 0; list-style: none; }
.picker-list li { padding-block: 8px; border-bottom: 1px solid var(--border); }
.picker-list label { display: flex; align-items: center; gap: 8px; overflow-wrap: anywhere; }
.picker-list .status { margin-left: auto; flex: none; }
</style>
