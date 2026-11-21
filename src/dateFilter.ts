export type DateRange = { mode: "range"; from: string | null; to: string | null };
export type DateFilter = { mode: "all" } | { mode: "unknown" } | DateRange;

export const ALL_DATES: DateFilter = { mode: "all" };
export const UNKNOWN_DATES: DateFilter = { mode: "unknown" };

export function sameDateFilter(left: DateFilter, right: DateFilter) {
  return left.mode === right.mode && (left.mode !== "range" ||
    (right.mode === "range" && left.from === right.from && left.to === right.to));
}

export function localDateString(date: Date) {
  const year = String(date.getFullYear()).padStart(4, "0");
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function lastDayOfMonth(year: number, month: number) {
  const date = new Date(0);
  date.setUTCFullYear(year, month, 0);
  return date.getUTCDate();
}

export function validDate(value: string) {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return false;
  const [year, month, day] = value.split("-").map(Number);
  return year > 0 && month >= 1 && month <= 12 && day >= 1 && day <= lastDayOfMonth(year, month);
}

export function calendarDate(value: string) {
  if (!validDate(value)) return undefined;
  const [year, month, day] = value.split("-").map(Number);
  const date = new Date(0);
  date.setHours(12, 0, 0, 0);
  date.setFullYear(year, month - 1, day);
  return date;
}

export function monthDateRange(month: string): DateRange {
  const [year, number] = month.split("-").map(Number);
  return { mode: "range", from: `${month}-01`, to: `${month}-${lastDayOfMonth(year, number)}` };
}

export function recentDateRange(today: Date, days: number): DateRange {
  const firstDay = new Date(0);
  firstDay.setUTCFullYear(today.getFullYear(), today.getMonth(), today.getDate() - days + 1);
  return { mode: "range", from: firstDay.toISOString().slice(0, 10), to: localDateString(today) };
}
