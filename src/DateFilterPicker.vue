<script setup lang="ts">
import { computed, nextTick, reactive, ref, shallowRef, useId } from "vue";
import { ElButton, ElDatePickerPanel, ElDialog, ElIcon } from "element-plus";
import { Calendar, ArrowDown } from "@element-plus/icons-vue";
import "element-plus/es/components/button/style/css";
import "element-plus/es/components/date-picker-panel/style/css";
import "element-plus/es/components/dialog/style/css";
import "element-plus/es/components/icon/style/css";
import { ALL_DATES, UNKNOWN_DATES, calendarDate, localDateString, monthDateRange, recentDateRange, validDate, type DateFilter } from "./dateFilter";

const props = defineProps<{ modelValue: DateFilter }>();
const emit = defineEmits<{ "update:modelValue": [value: DateFilter] }>();

const fields = [
  { key: "from", label: "起始日期" },
  { key: "to", label: "结束日期" },
] as const;
type Bound = typeof fields[number]["key"];

const id = useId();
const trigger = ref<HTMLButtonElement>();
const calendar = ref<HTMLElement>();
const open = ref(false);
const draft = reactive({ from: "", to: "" });
const touched = reactive({ from: false, to: false });
const activeBound = ref<Bound>("from");
const calendarCursor = shallowRef(new Date());

const summary = computed(() => {
  const filter = props.modelValue;
  if (filter.mode === "unknown") return "日期未知";
  if (filter.mode === "all") return "全部日期";
  if (!filter.from) return `截至 ${filter.to}`;
  if (!filter.to) return `自 ${filter.from} 起`;
  if (filter.from === filter.to) return filter.from;
  return `${filter.from} 至 ${filter.to}`;
});

const from = computed(() => draft.from.trim());
const to = computed(() => draft.to.trim());
const fromValid = computed(() => validDate(from.value));
const toValid = computed(() => validDate(to.value));
const reversed = computed(() => fromValid.value && toValid.value && from.value > to.value);
const errors = computed(() => ({
  from: touched.from && from.value && !fromValid.value ? "请输入有效的起始日期" : "",
  to: touched.to && to.value && !toValid.value ? "请输入有效的结束日期"
    : reversed.value ? "结束日期不能早于起始日期" : "",
}));

function openPicker() {
  const filter = props.modelValue;
  draft.from = filter.mode === "range" ? filter.from ?? "" : "";
  draft.to = filter.mode === "range" ? filter.to ?? "" : "";
  touched.from = false;
  touched.to = false;
  activeBound.value = draft.to && !draft.from ? "to" : "from";
  calendarCursor.value = calendarDate(draft[activeBound.value]) ?? new Date();
  open.value = true;
}

function selectBound(bound: Bound) {
  activeBound.value = bound;
  const date = calendarDate(draft[bound].trim());
  if (date) calendarCursor.value = date;
  void nextTick(() => calendar.value?.querySelector<HTMLElement>('td[tabindex="0"]')?.focus());
}

function moveCalendarCursor(value: unknown) {
  if (value instanceof Date && Number.isFinite(value.getTime())) calendarCursor.value = value;
}

function selectCalendarDay(event: MouseEvent | KeyboardEvent) {
  const keyboard = event instanceof KeyboardEvent;
  if (keyboard && (event.isComposing || (event.key !== "Enter" && event.key !== " "))) return;
  const cell = (event.target as HTMLElement).closest(".el-date-table td");
  const day = cell?.querySelector<HTMLElement>("[data-date]")?.dataset.date;
  if (!day || !validDate(day)) return;
  if (keyboard) {
    event.preventDefault();
    event.stopPropagation();
  }
  // The calendar also emits model updates for keyboard navigation. Only activate an endpoint on an explicit day selection.
  draft[activeBound.value] = day;
  if (activeBound.value === "from") activeBound.value = "to";
}

function dateCellClass(date: Date) {
  const day = localDateString(date);
  if (fromValid.value && day === from.value) return toValid.value && day < to.value ? "date-filter-endpoint date-filter-start" : "date-filter-endpoint";
  if (toValid.value && day === to.value) return fromValid.value && from.value < day ? "date-filter-endpoint date-filter-end" : "date-filter-endpoint";
  return fromValid.value && toValid.value && from.value < day && day < to.value ? "date-filter-in-range" : "";
}

function commit(value: DateFilter) {
  emit("update:modelValue", value);
  open.value = false;
}

function apply(event?: KeyboardEvent) {
  if (event?.isComposing) return;
  event?.preventDefault();
  touched.from = true;
  touched.to = true;
  const invalidField = fields.find(({ key }) => errors.value[key]);
  if (invalidField) {
    void nextTick(() => document.getElementById(`${id}-${invalidField.key}`)?.focus());
    return;
  }

  commit(from.value || to.value ? { mode: "range", from: from.value || null, to: to.value || null } : ALL_DATES);
}

function restoreFocus() {
  if (trigger.value?.getClientRects().length) trigger.value.focus({ preventScroll: true });
}
</script>

<template>
  <button ref="trigger" type="button" class="secondary date-filter-trigger" :class="{ 'has-date-filter': modelValue.mode !== 'all' }"
    aria-haspopup="dialog" :aria-expanded="open" :aria-label="`拍摄日期，${summary}`" @click="openPicker">
    <ElIcon aria-hidden="true"><Calendar /></ElIcon>
    <span class="date-filter-label">拍摄日期</span>
    <span class="date-filter-summary">{{ summary }}</span>
    <ElIcon class="date-filter-arrow" aria-hidden="true"><ArrowDown /></ElIcon>
  </button>

  <ElDialog v-model="open" class="date-filter-dialog" title="拍摄日期" width="420px" align-center append-to-body destroy-on-close
    @closed="restoreFocus">
    <div class="date-filter-presets" role="group" aria-label="日期快捷筛选">
      <ElButton :type="modelValue.mode === 'all' ? 'primary' : undefined" :plain="modelValue.mode === 'all'"
        @click="commit(ALL_DATES)">全部日期</ElButton>
      <ElButton @click="commit(recentDateRange(new Date(), 7))">最近 7 天</ElButton>
      <ElButton @click="commit(recentDateRange(new Date(), 30))">最近 30 天</ElButton>
      <ElButton @click="commit(monthDateRange(localDateString(new Date()).slice(0, 7)))">本月</ElButton>
      <ElButton :type="modelValue.mode === 'unknown' ? 'primary' : undefined" :plain="modelValue.mode === 'unknown'"
        @click="commit(UNKNOWN_DATES)">日期未知</ElButton>
    </div>

    <div class="date-filter-custom" role="group" aria-label="自选日期范围">
      <p :id="`${id}-hint`" class="date-filter-hint">输入 YYYY-MM-DD，或在日历中选取。起止日期可只填一项。</p>
      <div class="date-filter-fields">
        <div v-for="{ key, label } in fields" :key="key" class="date-filter-field" :class="{ 'is-date-target': key === activeBound }">
          <label :for="`${id}-${key}`">{{ label }}</label>
          <div class="date-filter-input-row">
            <input :id="`${id}-${key}`" v-model="draft[key]" type="text" placeholder="YYYY-MM-DD" autocomplete="off" spellcheck="false"
              :aria-invalid="!!errors[key]" :aria-describedby="errors[key] ? `${id}-hint ${id}-${key}-error` : `${id}-hint`"
              @blur="touched[key] = true" @keydown.enter="apply" />
            <button type="button" class="secondary date-filter-field-button" :aria-label="`在日历中选择${label}`"
              :aria-pressed="key === activeBound" @click="selectBound(key)">
              <ElIcon aria-hidden="true"><Calendar /></ElIcon>
            </button>
          </div>
          <p v-if="errors[key]" :id="`${id}-${key}-error`" class="date-filter-error" aria-live="polite">{{ errors[key] }}</p>
        </div>
      </div>
      <div ref="calendar" class="date-filter-calendar" role="group" :aria-label="activeBound === 'from' ? '起始日期日历' : '结束日期日历'"
        @click="selectCalendarDay" @keydown.capture="selectCalendarDay">
        <ElDatePickerPanel type="date" :model-value="calendarCursor" :cell-class-name="dateCellClass" :show-footer="false"
          @update:model-value="moveCalendarCursor">
          <template #default="cell">
            <div class="el-date-table-cell">
              <span class="el-date-table-cell__text" :data-date="cell.dayjs ? localDateString(cell.dayjs.toDate()) : undefined">{{ cell.renderText ?? cell.text }}</span>
            </div>
          </template>
        </ElDatePickerPanel>
      </div>
    </div>

    <template #footer>
      <ElButton @click="open = false">取消</ElButton>
      <ElButton type="primary" @click="apply()">应用</ElButton>
    </template>
  </ElDialog>
</template>

<style scoped>
.date-filter-trigger {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  max-width: 100%;
  min-width: 0;
  padding: 7px 11px;
  border-color: var(--control-border);
  line-height: 1.5;
  text-align: left;
}
.date-filter-trigger.has-date-filter {
  border-color: color-mix(in srgb, var(--accent) 45%, var(--control-border));
  background: var(--accent-soft);
  color: var(--accent);
}
.date-filter-trigger .el-icon { flex: none; }
.date-filter-label { flex: none; color: var(--muted); font-size: 13px; font-weight: 400; }
.date-filter-summary { min-width: 0; font-variant-numeric: tabular-nums; overflow-wrap: anywhere; }
.date-filter-arrow { margin-left: auto; }

:global(.date-filter-dialog.el-dialog) {
  display: flex;
  flex-direction: column;
  max-height: calc(100dvh - 32px);
  --el-dialog-padding-primary: 20px;
}
:global(.date-filter-dialog .el-dialog__body) {
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  scrollbar-width: thin;
  scrollbar-color: var(--control-border) transparent;
  padding: 3px;
  margin: -3px;
}
:global(.date-filter-dialog .el-dialog__footer) {
  display: flex;
  flex: none;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 16px;
  padding-top: 14px;
  border-top: 1px solid var(--border);
}
.date-filter-presets { display: flex; flex-wrap: wrap; gap: 6px; }
.date-filter-presets .el-button { height: 32px; padding: 6px 8px; font-size: 13px; }
.date-filter-custom { margin-top: 16px; padding-top: 12px; border-top: 1px solid var(--border); }
.date-filter-hint { margin: 0 0 10px; color: var(--muted); font-size: 12px; }
.date-filter-fields { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px 12px; align-items: start; }
.date-filter-field label { margin-bottom: 4px; color: var(--text-secondary); font-size: 13px; }
.date-filter-field.is-date-target label { color: var(--accent); }
.date-filter-input-row { display: flex; align-items: center; gap: 4px; }
.date-filter-input-row input { flex: 1; width: 0; padding-inline: 8px; font-variant-numeric: tabular-nums; }
.date-filter-input-row input[aria-invalid="true"] { border-color: var(--danger); }
.date-filter-field-button { display: grid; width: 36px; flex: none; place-items: center; padding: 0; border-color: var(--control-border); }
.date-filter-field-button[aria-pressed="true"] { border-color: var(--accent); background: var(--accent-soft); color: var(--accent); }
.date-filter-error { margin: 4px 0 0; color: var(--danger); font-size: 12px; }

.date-filter-calendar { margin-top: 12px; }
.date-filter-calendar :deep(.el-date-picker) { width: 100%; border-color: var(--border); border-radius: 8px; }
.date-filter-calendar :deep(.el-date-picker__header) { margin: 0; padding: 8px 8px 0; }
.date-filter-calendar :deep(.el-picker-panel__icon-btn) { width: 28px; height: 32px; margin-top: 0; }
.date-filter-calendar :deep(.el-date-picker__header-label) { display: inline-block; line-height: 32px; }
.date-filter-calendar :deep(.el-picker-panel__content) { width: auto; margin: 6px 8px 8px; }
.date-filter-calendar :deep(.el-date-table td) { width: auto; height: 34px; padding: 2px 0; }
.date-filter-calendar :deep(.el-date-table-cell__text) { width: 28px; height: 24px; }
.date-filter-calendar :deep(.el-date-table td.date-filter-in-range .el-date-table-cell) { background: var(--accent-soft); }
.date-filter-calendar :deep(.el-date-table td.date-filter-start .el-date-table-cell) { background: linear-gradient(to right, transparent 50%, var(--accent-soft) 50%); }
.date-filter-calendar :deep(.el-date-table td.date-filter-end .el-date-table-cell) { background: linear-gradient(to right, var(--accent-soft) 50%, transparent 50%); }
.date-filter-calendar :deep(.el-date-table td.current:not(.disabled):not(.date-filter-endpoint) .el-date-table-cell__text) { background: transparent; color: inherit; }
.date-filter-calendar :deep(.el-date-table td.today .el-date-table-cell__text) { box-shadow: inset 0 0 0 1px var(--accent); }
.date-filter-calendar :deep(.el-date-table td.date-filter-endpoint .el-date-table-cell__text) { background: var(--accent); color: var(--on-accent); }
.date-filter-calendar :deep(.el-date-table td.today.date-filter-endpoint .el-date-table-cell__text) { box-shadow: inset 0 0 0 1px var(--on-accent); }
.date-filter-calendar :deep(.el-date-table td:focus-visible .el-date-table-cell__text),
.date-filter-calendar :deep(.el-date-picker__header-label:focus-visible),
.date-filter-calendar :deep(.el-picker-panel__icon-btn:focus-visible) { outline: 2px solid var(--accent); outline-offset: 1px; }

@media (max-width: 639px) {
  .date-filter-trigger { width: 100%; }
}
@media (max-height: 680px) {
  :global(.date-filter-dialog.el-dialog) { --el-dialog-padding-primary: 16px; }
  :global(.date-filter-dialog .el-dialog__header) { padding-bottom: 12px; }
  :global(.date-filter-dialog .el-dialog__footer) { margin-top: 12px; padding-top: 10px; }
  .date-filter-custom { margin-top: 12px; padding-top: 10px; }
  .date-filter-calendar { margin-top: 10px; }
  .date-filter-calendar :deep(.el-picker-panel__content) { margin-block: 4px 6px; }
  .date-filter-calendar :deep(.el-date-table td) { height: 30px; padding-block: 0; }
}
@media (max-width: 440px) {
  :global(.date-filter-dialog.el-dialog) { --el-dialog-padding-primary: 14px; }
  .date-filter-fields { grid-template-columns: minmax(0, 1fr); }
}
</style>
