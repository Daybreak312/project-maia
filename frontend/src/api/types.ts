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
