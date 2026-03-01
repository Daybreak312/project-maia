import type {
  Document,
  IngestResponse,
  SearchResponse,
  SearchOptions,
  ListResponse,
  ListOptions,
  SettingsResponse,
} from './types';

const API_BASE = '';

async function request<T>(endpoint: string, options?: RequestInit): Promise<T> {
  const response = await fetch(`${API_BASE}${endpoint}`, {
    headers: {
      'Content-Type': 'application/json',
      ...options?.headers,
    },
    ...options,
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
    request<IngestResponse>('/ingest', {
      method: 'POST',
      body: JSON.stringify({ content }),
    }),

  // Search with options
  search: (options: SearchOptions) =>
    request<SearchResponse>('/search', {
      method: 'POST',
      body: JSON.stringify({
        query: options.query,
        limit: options.limit ?? 10,
        offset: options.offset ?? 0,
        mode: options.mode ?? 'hybrid',
        tags: options.tags,
      }),
    }),

  // Documents
  getDocument: (id: string) => request<Document>(`/documents/${id}`),

  updateDocument: (id: string, content: string) =>
    request<IngestResponse>(`/documents/${id}`, {
      method: 'PUT',
      body: JSON.stringify({ content }),
    }),

  deleteDocument: (id: string) =>
    request<void>(`/documents/${id}`, {
      method: 'DELETE',
    }),

  // List with pagination and filtering
  getRecent: (options: ListOptions = {}) => {
    const params = new URLSearchParams();
    params.set('limit', String(options.limit ?? 20));
    params.set('offset', String(options.offset ?? 0));
    if (options.tags && options.tags.length > 0) {
      params.set('tags', options.tags.join(','));
    }
    return request<ListResponse>(`/recent?${params.toString()}`);
  },

  // Tags
  getTags: () => request<string[]>('/tags'),

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

  // Reindex
  reindex: () =>
    request<{ indexed: number }>('/api/reindex', {
      method: 'POST',
    }),
};
