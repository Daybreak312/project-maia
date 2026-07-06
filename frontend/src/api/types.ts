export interface Entity {
  entity_type: string | { other: string };
  value: string;
  context?: string;
}

export function getEntityTypeLabel(entityType: string | { other: string }): string {
  if (typeof entityType === 'string') {
    return entityType;
  }
  return entityType.other || 'other';
}

export interface Document {
  id: string;
  raw_content: string;
  summary: string;
  entities: Entity[];
  created_at: string;
}

export interface IngestResponse {
  id: string;
  summary: string;
  entities: Entity[];
}

export interface SearchResult {
  id: string;
  summary: string;
  relevance_score: number;
  /** 결과 출처 워크스페이스 (교차 검색 시 구분) */
  workspace?: string;
}

export interface SearchResponse {
  results: SearchResult[];
  sources_used: string[];
  total: number;
  mode: string;
}

export interface SearchOptions {
  query: string;
  limit?: number;
  offset?: number;
  mode?: 'vector' | 'keyword' | 'hybrid';
}

export interface ListResponse {
  documents: Document[];
  total: number;
  limit: number;
  offset: number;
}

export interface ListOptions {
  limit?: number;
  offset?: number;
}

export interface ProviderInfo {
  provider: string;
  has_api_key: boolean;
  api_key_preview: string | null;
}

export interface SettingsResponse {
  providers: ProviderInfo[];
  parsing_provider: string | null;
  embedding_provider: string | null;
}

// ─── 워크스페이스 ────────────────────────────────────────────────
export type WorkspaceTemplate = 'personal' | 'enterprise';

/** 백엔드 WorkspaceConfig 중 프론트에서 사용하는 필드 (전체 구조의 부분집합). */
export interface WorkspaceSummary {
  id: string;
  name: string;
  template: WorkspaceTemplate;
  created_at: string;
  search: {
    cross_workspace: string[];
    default_mode: string;
    time_decay_lambda: number;
  };
}

export interface CreateWorkspaceRequest {
  id: string;
  name: string;
  template?: WorkspaceTemplate;
}

// ─── API 키 ──────────────────────────────────────────────────────
export type Permission = 'read_only' | 'read_write' | 'admin';

/** 키 공개 뷰 (해시 미포함). */
export interface ApiKeyInfo {
  key_id: string;
  label: string;
  workspaces: string[];
  permissions: Permission;
  created_at: string;
  last_used_at: string | null;
  expires_at: string | null;
}

export interface CreateKeyRequest {
  label: string;
  workspaces: string[];
  permissions: Permission;
  expires_at?: string | null;
}

export interface CreateKeyResponse {
  /** 평문 키 — 이 응답에서만 확인 가능 */
  api_key: string;
  key: ApiKeyInfo;
}
