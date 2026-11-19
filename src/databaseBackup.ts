export interface BackupEntry { name: string; size: number; sha256: string }
export interface BackupInspection {
  manifest: { formatVersion: number; appVersion: string; createdAt: string; sourceAlbum: string; databases: BackupEntry[] };
  fingerprint: string;
}
export interface LibrarySummary {
  scanId: number; total: number; issues: number; thumbnailConcurrency: number; preferenceWarning: string | null;
}
export interface DatabaseRestoreResult {
  directory: string; rollbackPath: string; summary: LibrarySummary | null; reopenError: string | null; warning: string | null;
}
