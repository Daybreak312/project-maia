import type {
  Document,
  IngestResponse,
  NeighborsResponse,
  SearchResponse,
  SearchOptions,
  ListResponse,
  ListOptions,
  SettingsResponse,
  WorkspaceSummary,
  CreateWorkspaceRequest,
  ApiKeyInfo,
  CreateKeyRequest,
  CreateKeyResponse,
  ConnectorView,
  ConnectorInstance,
  RegisterConnectorRequest,
  SyncTriggerRequest,
  ReviewItem,
  ReviewStatus,
  DetectorKind,
  ReviewDecision,
  JudgeResponse,
  PatrolRun,
  PatrolState,
  DailyRollup,
} from './types';

const API_BASE = '';

// ─── 워크스페이스 & 웹 UI 인증 키 상태 (localStorage 영속) ───────────────
// 별도 상태 라이브러리 없이 client 모듈이 단일 진실 원천 역할을 한다.
const WORKSPACE_KEY = 'maia_workspace';
const AUTH_KEY = 'maia_auth_key';

let currentWorkspace = localStorage.getItem(WORKSPACE_KEY) || '';
let authKey = localStorage.getItem(AUTH_KEY) || '';

/** 현재 선택된 워크스페이스. 빈 문자열이면 서버의 키 기본값을 사용한다. */
export function getWorkspace(): string {
  return currentWorkspace;
}

export function setWorkspace(id: string): void {
  currentWorkspace = id;
  if (id) {
    localStorage.setItem(WORKSPACE_KEY, id);
  } else {
    localStorage.removeItem(WORKSPACE_KEY);
  }
}

/** 웹 UI가 admin 작업에 사용할 Bearer 키 (마스터키 또는 admin 키). */
export function getAuthKey(): string {
  return authKey;
}

export function setAuthKey(key: string): void {
  authKey = key;
  if (key) {
    localStorage.setItem(AUTH_KEY, key);
  } else {
    localStorage.removeItem(AUTH_KEY);
  }
}

/** 현재 워크스페이스를 쿼리 파라미터로 덧붙인다. */
function withWorkspace(path: string): string {
  if (!currentWorkspace) return path;
  const sep = path.includes('?') ? '&' : '?';
  return `${path}${sep}workspace=${encodeURIComponent(currentWorkspace)}`;
}

async function request<T>(endpoint: string, options?: RequestInit): Promise<T> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...(options?.headers as Record<string, string>),
  };
  if (authKey) {
    headers['Authorization'] = `Bearer ${authKey}`;
  }

  const response = await fetch(`${API_BASE}${endpoint}`, {
    ...options,
    headers,
  });

  if (!response.ok) {
    const text = await response.text();
    throw new Error(text || `HTTP ${response.status}`);
  }

  if (response.status === 204) {
    return undefined as T;
  }

  return response.json();
}

export const api = {
  // Ingest
  ingest: (content: string) =>
    request<IngestResponse>(withWorkspace('/ingest'), {
      method: 'POST',
      body: JSON.stringify({ content }),
    }),

  // Search with options
  search: (options: SearchOptions) =>
    request<SearchResponse>(withWorkspace('/search'), {
      method: 'POST',
      body: JSON.stringify({
        query: options.query,
        limit: options.limit ?? 10,
        offset: options.offset ?? 0,
        mode: options.mode ?? 'hybrid',
      }),
    }),

  // Documents
  getDocument: (id: string) => request<Document>(withWorkspace(`/documents/${id}`)),

  // 그래프 이웃 조회 (연결된 문서)
  getNeighbors: (id: string, depth = 1) =>
    request<NeighborsResponse>(
      withWorkspace(`/documents/${id}/neighbors?depth=${depth}`),
    ),

  updateDocument: (id: string, content: string) =>
    request<IngestResponse>(withWorkspace(`/documents/${id}`), {
      method: 'PUT',
      body: JSON.stringify({ content }),
    }),

  deleteDocument: (id: string) =>
    request<void>(withWorkspace(`/documents/${id}`), {
      method: 'DELETE',
    }),

  // List with pagination and filtering
  getRecent: (options: ListOptions = {}) => {
    const params = new URLSearchParams();
    params.set('limit', String(options.limit ?? 20));
    params.set('offset', String(options.offset ?? 0));
    return request<ListResponse>(withWorkspace(`/recent?${params.toString()}`));
  },

  // Settings
  getSettings: () => request<SettingsResponse>('/api/settings'),

  updateSettings: (data: { parsing_provider?: string; embedding_provider?: string }) =>
    request<void>('/api/settings', {
      method: 'PUT',
      body: JSON.stringify(data),
    }),

  setApiKey: (provider: string, apiKey: string) =>
    request<void>(`/api/settings/models/${provider}/key`, {
      method: 'POST',
      body: JSON.stringify({ api_key: apiKey }),
    }),

  deleteApiKey: (provider: string) =>
    request<void>(`/api/settings/models/${provider}/key`, {
      method: 'DELETE',
    }),

  testApiKey: (provider: string) =>
    request<{ valid: boolean; message?: string }>(`/api/settings/models/${provider}/test`, {
      method: 'POST',
    }),

  // Codex auth.json 임포트 (원문 문자열을 래퍼로 전달 — 서버가 파싱/검증)
  importCodex: (authJson: string) =>
    request<SettingsResponse>('/api/settings/models/codex/import', {
      method: 'POST',
      body: JSON.stringify({ auth_json: authJson }),
    }),

  // Reindex (현재 워크스페이스 대상)
  reindex: () =>
    request<{ indexed: number }>(withWorkspace('/api/reindex'), {
      method: 'POST',
    }),

  // ─── 워크스페이스 관리 (admin) ───────────────────────────────
  listWorkspaces: () => request<WorkspaceSummary[]>('/api/workspaces'),

  createWorkspace: (body: CreateWorkspaceRequest) =>
    request<WorkspaceSummary>('/api/workspaces', {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  deleteWorkspace: (id: string) =>
    request<void>(`/api/workspaces/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    }),

  // ─── API 키 관리 (admin) ─────────────────────────────────────
  listKeys: () => request<ApiKeyInfo[]>('/api/keys'),

  createKey: (body: CreateKeyRequest) =>
    request<CreateKeyResponse>('/api/keys', {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  revokeKey: (keyId: string) =>
    request<void>(`/api/keys/${encodeURIComponent(keyId)}`, {
      method: 'DELETE',
    }),

  // ─── 커넥터 관리 (workspace 명시) ────────────────────────────
  listConnectors: (workspace: string) =>
    request<ConnectorView[]>(`/api/connectors?workspace=${encodeURIComponent(workspace)}`),

  registerConnector: (workspace: string, body: RegisterConnectorRequest) =>
    request<ConnectorInstance>(`/api/connectors?workspace=${encodeURIComponent(workspace)}`, {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  deleteConnector: (workspace: string, id: string) =>
    request<void>(
      `/api/connectors/${encodeURIComponent(id)}?workspace=${encodeURIComponent(workspace)}`,
      { method: 'DELETE' },
    ),

  triggerConnectorSync: (workspace: string, id: string, body: SyncTriggerRequest = {}) =>
    request<{ status: string; workspace: string; connector_id: string }>(
      `/api/connectors/${encodeURIComponent(id)}/sync?workspace=${encodeURIComponent(workspace)}`,
      { method: 'POST', body: JSON.stringify(body) },
    ),

  getConnectorStatus: (workspace: string, id: string) =>
    request<ConnectorView>(
      `/api/connectors/${encodeURIComponent(id)}/status?workspace=${encodeURIComponent(workspace)}`,
    ),

  // ─── Patrol · 거버넌스 (현재 워크스페이스 대상) ───────────────
  runPatrol: () =>
    request<PatrolRun>(withWorkspace('/api/patrol/run'), { method: 'POST' }),

  getPatrolHistory: () => request<PatrolState>(withWorkspace('/api/patrol/history')),

  listReviews: (status?: ReviewStatus, kind?: DetectorKind) => {
    const params = new URLSearchParams();
    if (status) params.set('status', status);
    if (kind) params.set('kind', kind);
    const qs = params.toString();
    return request<ReviewItem[]>(withWorkspace(`/api/review${qs ? `?${qs}` : ''}`));
  },

  judgeReviews: (ids: string[], decision: ReviewDecision) =>
    request<JudgeResponse>(withWorkspace('/api/review/judge'), {
      method: 'POST',
      body: JSON.stringify({ ids, decision }),
    }),

  submitFeedback: (query: string, documentId: string) =>
    request<void>(withWorkspace('/api/feedback'), {
      method: 'POST',
      body: JSON.stringify({ query, document_id: documentId }),
    }),

  getMetrics: (from?: string, until?: string) => {
    const params = new URLSearchParams();
    if (from) params.set('from', from);
    if (until) params.set('until', until);
    const qs = params.toString();
    return request<DailyRollup[]>(withWorkspace(`/api/metrics${qs ? `?${qs}` : ''}`));
  },
};
