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
  MeResponse,
  LoginResponse,
  UserInfo,
  CreateUserRequest,
  CreateUserResponse,
  MembersResponse,
  SetVisibilityRequest,
  Permission,
} from './types';

const API_BASE = '';

// ─── 워크스페이스 & 웹 UI 인증 키 상태 (localStorage 영속) ───────────────
// 별도 상태 라이브러리 없이 client 모듈이 단일 진실 원천 역할을 한다.
const WORKSPACE_KEY = 'maia_workspace';
const AUTH_KEY = 'maia_auth_key';
// 로그아웃 후 저장 키의 "자동 사용"만 중단하는 플래그. 키 자체는 파괴하지
// 않는다 — 로그인 화면의 "저장된 API 키로 계속"으로 명시 복귀할 수 있다.
const AUTH_KEY_SUSPENDED = 'maia_auth_key_suspended';

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
    // 새 키를 명시 저장하면 자동 사용 중단 상태를 해제한다.
    localStorage.removeItem(AUTH_KEY_SUSPENDED);
  } else {
    localStorage.removeItem(AUTH_KEY);
    localStorage.removeItem(AUTH_KEY_SUSPENDED);
  }
}

// ─── 인증 모드 (세션 vs Bearer 키) ───────────────────────────────────────
// 백엔드 해석 순서는 마스터키/API 키(Bearer) → 세션 쿠키다. 즉 유효한 Bearer가
// 실리면 세션이 무시되므로, "세션 우선" 정책은 클라이언트가 세션 모드에서
// Authorization 헤더를 생략하는 것으로 구현한다.
export type AuthMode = 'session' | 'key';

let authMode: AuthMode | null = null;

export function getAuthMode(): AuthMode | null {
  return authMode;
}

function isKeySuspended(): boolean {
  return localStorage.getItem(AUTH_KEY_SUSPENDED) === '1';
}

/** 401 수신 시 앱이 인증 상태를 재산출하도록 등록하는 훅 (App에서 설정). */
let onUnauthorized: (() => void) | null = null;

export function setOnUnauthorized(handler: (() => void) | null): void {
  onUnauthorized = handler;
}

/** HTTP 오류 — 상태 코드로 401(재인증)과 403(권한 부족)을 구분할 수 있게 한다. */
export class ApiError extends Error {
  status: number;

  constructor(status: number, message: string) {
    super(message || `HTTP ${status}`);
    this.name = 'ApiError';
    this.status = status;
  }
}

/** 현재 워크스페이스를 쿼리 파라미터로 덧붙인다. */
function withWorkspace(path: string): string {
  if (!currentWorkspace) return path;
  const sep = path.includes('?') ? '&' : '?';
  return `${path}${sep}workspace=${encodeURIComponent(currentWorkspace)}`;
}

/** 공통 fetch. `bearerKey`가 주어지면 그 키로, 아니면 쿠키만으로 요청한다. */
async function rawRequest<T>(
  endpoint: string,
  options?: RequestInit,
  bearerKey?: string,
): Promise<T> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    ...(options?.headers as Record<string, string>),
  };
  if (bearerKey) {
    headers['Authorization'] = `Bearer ${bearerKey}`;
  }

  const response = await fetch(`${API_BASE}${endpoint}`, {
    ...options,
    headers,
  });

  if (!response.ok) {
    const text = await response.text();
    throw new ApiError(response.status, text);
  }

  if (response.status === 204) {
    return undefined as T;
  }

  return response.json();
}

/**
 * 일반 API 요청. 키 모드에서만 Bearer를 싣고, 세션 모드에서는 쿠키에 맡긴다.
 * 401이면 onUnauthorized 훅을 발화해 앱이 로그인 화면으로 전환하게 한다
 * (fail-closed — 세션 만료·키 폐기가 조용히 무시되지 않는다).
 */
async function request<T>(endpoint: string, options?: RequestInit): Promise<T> {
  try {
    return await rawRequest<T>(
      endpoint,
      options,
      authMode === 'key' && authKey ? authKey : undefined,
    );
  } catch (err) {
    if (err instanceof ApiError && err.status === 401) {
      onUnauthorized?.();
    }
    throw err;
  }
}

// ─── 인증 수명주기 (부트스트랩 · 로그인 · 로그아웃) ──────────────────────
// 아래 함수들은 401 훅을 발화하지 않는 rawRequest를 쓴다 — 인증 상태를
// 산출하는 중의 401은 "결과"이지 "이상 신호"가 아니기 때문.

/**
 * 앱 시작 시 인증 상태 산출: 세션(쿠키) 우선 → 저장 키 폴백.
 * 로그아웃으로 자동 사용이 중단된 키는 폴백에서 제외한다.
 * 어느 쪽도 유효하지 않으면 null (→ 로그인 화면).
 */
export async function bootstrapAuth(): Promise<MeResponse | null> {
  try {
    const me = await rawRequest<MeResponse>('/api/auth/me');
    authMode = 'session';
    return me;
  } catch {
    // 세션 없음/만료 — 키 폴백으로 진행
  }

  if (authKey && !isKeySuspended()) {
    try {
      const me = await rawRequest<MeResponse>('/api/auth/me', undefined, authKey);
      authMode = 'key';
      return me;
    } catch {
      // 무효 키 — 로그인 화면에서 재입력하게 둔다 (자동 삭제하지 않음)
    }
  }

  authMode = null;
  return null;
}

/** ID/PW 로그인 → 세션 쿠키 발급 → me 재조회. 실패는 그대로 throw (단일 401). */
export async function loginWithPassword(
  username: string,
  password: string,
): Promise<MeResponse> {
  await rawRequest<LoginResponse>('/api/auth/login', {
    method: 'POST',
    body: JSON.stringify({ username, password }),
  });
  const me = await rawRequest<MeResponse>('/api/auth/me');
  authMode = 'session';
  return me;
}

/** API 키 검증 후 키 모드로 진입. 검증 성공 시에만 localStorage에 영속한다. */
export async function loginWithKey(key: string): Promise<MeResponse> {
  const me = await rawRequest<MeResponse>('/api/auth/me', undefined, key);
  setAuthKey(key);
  authMode = 'key';
  return me;
}

/**
 * 로그아웃: 서버 세션 폐기(멱등) + 저장 키 자동 사용 중단 + 모드 초기화.
 * 저장 키는 지우지 않는다 — 파괴는 Admin의 키 관리에서만 명시적으로.
 */
export async function logoutAuth(): Promise<void> {
  try {
    await rawRequest<void>('/api/auth/logout', { method: 'POST' });
  } catch {
    // 서버 불가용이어도 클라이언트 상태는 정리한다 (fail-closed)
  }
  if (authKey) {
    localStorage.setItem(AUTH_KEY_SUSPENDED, '1');
  }
  authMode = null;
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

  // ─── 계정 관리 (글로벌 admin) ────────────────────────────────
  listUsers: () => request<UserInfo[]>('/api/users'),

  createUser: (body: CreateUserRequest) =>
    request<CreateUserResponse>('/api/users', {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  deleteUser: (userId: string) =>
    request<void>(`/api/users/${encodeURIComponent(userId)}`, {
      method: 'DELETE',
    }),

  // 비밀번호 변경 — 본인(세션) 또는 admin. 성공 시 대상 계정의 전 세션이
  // 폐기된다 (본인 변경이면 재로그인 필요).
  changePassword: (userId: string, password: string) =>
    request<void>(`/api/users/${encodeURIComponent(userId)}/password`, {
      method: 'PUT',
      body: JSON.stringify({ password }),
    }),

  // ─── 내 API 키 셀프서비스 (로그인 세션 전용 — 키 인증은 403) ──
  listMyKeys: () => request<ApiKeyInfo[]>('/api/me/keys'),

  createMyKey: (body: CreateKeyRequest) =>
    request<CreateKeyResponse>('/api/me/keys', {
      method: 'POST',
      body: JSON.stringify(body),
    }),

  revokeMyKey: (keyId: string) =>
    request<void>(`/api/me/keys/${encodeURIComponent(keyId)}`, {
      method: 'DELETE',
    }),

  // ─── 워크스페이스 멤버십 · 공개 설정 (글로벌 admin 또는 해당 ws admin) ──
  listMembers: (workspaceId: string) =>
    request<MembersResponse>(
      `/api/workspaces/${encodeURIComponent(workspaceId)}/members`,
    ),

  upsertMember: (workspaceId: string, userId: string, role: Permission) =>
    request<void>(
      `/api/workspaces/${encodeURIComponent(workspaceId)}/members/${encodeURIComponent(userId)}`,
      { method: 'PUT', body: JSON.stringify({ role }) },
    ),

  removeMember: (workspaceId: string, userId: string) =>
    request<void>(
      `/api/workspaces/${encodeURIComponent(workspaceId)}/members/${encodeURIComponent(userId)}`,
      { method: 'DELETE' },
    ),

  setVisibility: (workspaceId: string, body: SetVisibilityRequest) =>
    request<MembersResponse>(
      `/api/workspaces/${encodeURIComponent(workspaceId)}/visibility`,
      { method: 'PUT', body: JSON.stringify(body) },
    ),

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
