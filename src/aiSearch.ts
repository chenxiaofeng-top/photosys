export interface ModelStatus {
  directory: string | null;
  ready: boolean;
  totalBytes: number;
  resumableBytes: number;
  message: string | null;
  modelId: string;
  activeModelId: string;
  activeReady: boolean;
}

export interface ModelOption {
  id: string; kind: "search" | "people"; name: string; description: string; measurement: string; cpu: string; license: string; source: string; featureVersion: string; dimension: number; imageSize: number;
  assets: { size: number }[];
}

export interface IndexStatus {
  modelId: string; needsRebuild: boolean; rebuildReason: string | null;
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
