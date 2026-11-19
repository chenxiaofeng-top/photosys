export interface Progress {
  phase: "scanning" | "sampling" | "hashing" | "comparing";
  found: number;
  checked: number;
  total: number;
  bytesRead: number;
  currentFile: string;
  elapsedMs: number;
}
export interface Summary {
  scanId: number;
  scanned: number;
  groups: number;
  files: number;
  extras: number;
  extraBytes: number;
  issues: number;
  bytesRead: number;
  elapsedMs: number;
  selected: number;
  selectedBytes: number;
  hiddenSelected: number;
  revision: number;
}
export type Kind = "photo" | "video" | "other";
export interface Filter { query: string; kind: Kind | null }
export type Sort = "extraBytes" | "size" | "count";
export type KeepRule = "shallow" | "oldest" | "newest";
export interface FileRow {
  id: number;
  name: string;
  relativePath: string;
  modifiedAt: string;
  hardLink: boolean;
  size: number;
  kind: Kind;
  previewAvailable: boolean;
  selected: boolean;
  disabled: boolean;
}
export interface Group {
  id: number;
  size: number;
  count: number;
  selected: number;
  extraBytes: number;
  needsRescan: boolean;
  representative: FileRow;
}
export interface Page<T> { total: number; items: T[] }
export interface GroupPage extends Page<Group> { summary: Summary }
export interface ConfirmationRow { groupId: number; size: number; moving: boolean; file: FileRow }
export interface Confirmation extends Page<ConfirmationRow> { summary: Summary }
export interface Issue { path: string; reason: string }
export interface TrashOutcome { id: number; relativePath: string; status: "moved" | "skipped" | "failed"; reason: string | null }
export interface BatchSummary { moved: number; skipped: number; failed: number }
export type ScanOutcome = { status: "finished"; summary: Summary } | { status: "cancelled" };
export type SelectionAction = { kind: "toggle"; fileId: number } | { kind: "clear" } | { kind: "apply"; filter: Filter; rule: KeepRule; preferredDirectory: string };
