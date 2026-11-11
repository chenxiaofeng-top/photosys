<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { computed, onMounted, ref } from "vue";

const text = ref("");
const savedText = ref("");
const loading = ref(true);
const loaded = ref(false);
const saving = ref(false);
const error = ref("");
const dirty = computed(() => text.value !== savedText.value);

const status = computed(() => {
  if (loading.value) return "正在读取本地笔记…";
  if (saving.value) return "正在保存…";
  if (dirty.value) return "有未保存的修改";
  return "已与本地数据库同步";
});

async function loadNote() {
  loading.value = true;
  error.value = "";

  try {
    const storedText = await invoke<string>("load_note");
    text.value = storedText;
    savedText.value = storedText;
    loaded.value = true;
  } catch (cause) {
    error.value = `无法读取笔记。${String(cause)}`;
  } finally {
    loading.value = false;
  }
}

async function saveNote() {
  if (!loaded.value || saving.value || !dirty.value) return;

  const draft = text.value;
  saving.value = true;
  error.value = "";

  try {
    await invoke("save_note", { text: draft });
    savedText.value = draft;
  } catch (cause) {
    error.value = `无法保存笔记。${String(cause)}`;
  } finally {
    saving.value = false;
  }
}

onMounted(loadNote);
</script>

<template>
  <main>
    <header>
      <p class="app-name">PhotoSys</p>
      <h1>本地笔记</h1>
    </header>

    <form @submit.prevent="saveNote">
      <label for="note">笔记内容</label>
      <textarea id="note" v-model="text" rows="8" :disabled="!loaded" />

      <div class="actions">
        <p v-if="error" class="error" role="alert">{{ error }}</p>
        <p v-else role="status">{{ status }}</p>

        <button v-if="!loaded && !loading" type="button" @click="loadNote">
          重新读取
        </button>
        <button v-else type="submit" :disabled="!loaded || saving || !dirty">
          {{ saving ? "保存中…" : "保存" }}
        </button>
      </div>
    </form>
  </main>
</template>

<style>
:root {
  font-family: Inter, system-ui, sans-serif;
  color: #152e30;
  background: #eef4f1;
}

body {
  margin: 0;
}

main {
  max-width: 42rem;
  margin: 0 auto;
  padding: clamp(1.25rem, 6vw, 3.5rem);
}

header {
  margin-bottom: 1.5rem;
}

.app-name {
  margin: 0 0 0.5rem;
  color: #366560;
  font-weight: 700;
}

h1 {
  margin: 0;
  font-size: 2rem;
}

form {
  padding: clamp(1rem, 4vw, 1.75rem);
  background: #fff;
  border: 1px solid #c9d9d4;
  border-radius: 0.875rem;
}

label {
  display: block;
  margin-bottom: 0.5rem;
  font-weight: 650;
}

textarea {
  display: block;
  box-sizing: border-box;
  width: 100%;
  min-height: 8rem;
  padding: 0.75rem;
  resize: vertical;
  border: 1px solid #829d95;
  border-radius: 0.5rem;
  color: inherit;
  background: #fcfefd;
  font: inherit;
  line-height: 1.6;
}

.actions {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  justify-content: space-between;
  gap: 0.75rem;
  margin-top: 1rem;
}

.actions p {
  margin: 0;
  color: #395d57;
  overflow-wrap: anywhere;
}

.actions .error {
  color: #9c2626;
}

button {
  min-width: 6rem;
  min-height: 2.5rem;
  padding: 0.5rem 1rem;
  border: 0;
  border-radius: 0.5rem;
  background: #176252;
  color: #fff;
  font: inherit;
  font-weight: 650;
  cursor: pointer;
}

button:disabled {
  background: #dde8e3;
  color: #536863;
  cursor: default;
}

:is(button, textarea):focus-visible {
  outline: 2px solid #126353;
  outline-offset: 2px;
}
</style>
