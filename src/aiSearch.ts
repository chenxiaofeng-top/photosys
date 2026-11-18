export interface ModelStatus {
  directory: string | null;
  ready: boolean;
  totalBytes: number;
  resumableBytes: number;
  message: string | null;
  modelId: string;
}

export interface IndexStatus {
  scanId: number;
  enabled: boolean;
  running: boolean;
  total: number;
  indexed: number;
  failed: number;
  pending: number;
  reused: number;
  writable: boolean;
  message: string | null;
}
