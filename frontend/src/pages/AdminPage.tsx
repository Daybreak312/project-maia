import { useState, useEffect, useCallback } from 'react';
import type {
  CodexStatus,
  LocalStatus,
  ProviderInfo,
  SettingsResponse,
  WorkspaceSummary,
} from '../api/types';
import { api } from '../api/client';
import { ConfirmModal } from '../components/ConfirmModal';
import {
  WebUiKeySection,
  WorkspacesSection,
  ApiKeysSection,
  ConnectorsSection,
} from '../components/AdminManagement';

interface AdminPageProps {
  showToast: (message: string, type: 'success' | 'error') => void;
  /** 워크스페이스 생성/삭제 시 상위(App/Navbar) 목록을 갱신하기 위한 콜백 */
  onWorkspacesChanged: () => void;
}

function ProviderCard({
  provider,
  parsingProvider,
  embeddingProvider,
  onSetKey,
  onDeleteKey,
  onTestKey,
  onSetParsing,
  onSetEmbedding,
}: {
  provider: ProviderInfo;
  parsingProvider: string | null;
  embeddingProvider: string | null;
  onSetKey: (provider: string, key: string) => Promise<void>;
  onDeleteKey: (provider: string) => Promise<void>;
  onTestKey: (provider: string) => Promise<void>;
  onSetParsing: (provider: string) => Promise<void>;
  onSetEmbedding: (provider: string) => Promise<void>;
}) {
  const [apiKey, setApiKey] = useState('');
  const [isSettingKey, setIsSettingKey] = useState(false);
  const [isTesting, setIsTesting] = useState(false);
  const [showKeyInput, setShowKeyInput] = useState(false);

  const handleSetKey = async () => {
    if (!apiKey.trim()) return;
    setIsSettingKey(true);
    try {
      await onSetKey(provider.provider, apiKey.trim());
      setApiKey('');
      setShowKeyInput(false);
    } finally {
      setIsSettingKey(false);
    }
  };

  const handleTest = async () => {
    setIsTesting(true);
    try {
      await onTestKey(provider.provider);
    } finally {
      setIsTesting(false);
    }
  };

  const isParsing = parsingProvider === provider.provider;
  const isEmbedding = embeddingProvider === provider.provider;
  // Claude는 임베딩을 지원하지 않는다(파싱 전용). gemini/openai만 임베딩 선택 가능.
  const canEmbed = provider.provider !== 'claude';
  const isClaude = provider.provider === 'claude';

  return (
    <div className="bg-card border border-border rounded-lg p-6">
      <div className="flex justify-between items-start mb-4">
        <div>
          <h3 className="text-lg font-semibold text-gray-200 capitalize">
            {provider.provider}
          </h3>
          <p className="text-sm text-muted">
            {provider.has_api_key
              ? `API Key: ${provider.api_key_preview || '••••••••'}`
              : 'No API key configured'}
          </p>
          {isClaude && (
            <p className="text-xs text-muted mt-1">
              구독(setup-token)도 지원됩니다 — 터미널에서 <code className="text-primary">claude setup-token</code> 실행 후
              산출된 <code className="text-primary">sk-ant-oat…</code> 토큰을 위 키 입력에 그대로 붙여넣으세요.
            </p>
          )}
        </div>
        <div className="flex gap-2">
          {isParsing && (
            <span className="text-xs bg-primary/20 text-primary px-2 py-1 rounded">
              Parsing
            </span>
          )}
          {isEmbedding && (
            <span className="text-xs bg-success/20 text-success px-2 py-1 rounded">
              Embedding
            </span>
          )}
        </div>
      </div>

      {/* API Key Management */}
      {showKeyInput ? (
        <div className="flex gap-2 mb-4">
          <input
            type="password"
            className="flex-1 bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
            placeholder="Enter API key..."
            value={apiKey}
            onChange={(e) => setApiKey(e.target.value)}
          />
          <button
            className="px-4 py-2 bg-primary text-white text-sm rounded hover:bg-primary-hover transition-colors disabled:opacity-50"
            onClick={handleSetKey}
            disabled={isSettingKey || !apiKey.trim()}
          >
            {isSettingKey ? 'Saving...' : 'Save'}
          </button>
          <button
            className="px-4 py-2 bg-border text-gray-200 text-sm rounded hover:bg-muted transition-colors"
            onClick={() => {
              setShowKeyInput(false);
              setApiKey('');
            }}
          >
            Cancel
          </button>
        </div>
      ) : (
        <div className="flex gap-2 mb-4">
          <button
            className="px-4 py-2 bg-border text-gray-200 text-sm rounded hover:bg-muted transition-colors"
            onClick={() => setShowKeyInput(true)}
          >
            {provider.has_api_key ? 'Update Key' : 'Set Key'}
          </button>
          {provider.has_api_key && (
            <>
              <button
                className="px-4 py-2 bg-border text-gray-200 text-sm rounded hover:bg-muted transition-colors disabled:opacity-50"
                onClick={handleTest}
                disabled={isTesting}
              >
                {isTesting ? 'Testing...' : 'Test'}
              </button>
              <button
                className="px-4 py-2 bg-error text-white text-sm rounded hover:bg-red-700 transition-colors"
                onClick={() => onDeleteKey(provider.provider)}
              >
                Delete Key
              </button>
            </>
          )}
        </div>
      )}

      {/* Provider Selection */}
      {provider.has_api_key && (
        <div className="flex gap-2">
          <button
            className={`px-4 py-2 text-sm rounded transition-colors ${
              isParsing
                ? 'bg-primary text-white'
                : 'bg-border text-gray-200 hover:bg-muted'
            }`}
            onClick={() => onSetParsing(provider.provider)}
            disabled={isParsing}
          >
            Use for Parsing
          </button>
          {canEmbed && (
            <button
              className={`px-4 py-2 text-sm rounded transition-colors ${
                isEmbedding
                  ? 'bg-success text-white'
                  : 'bg-border text-gray-200 hover:bg-muted'
              }`}
              onClick={() => onSetEmbedding(provider.provider)}
              disabled={isEmbedding}
            >
              Use for Embedding
            </button>
          )}
        </div>
      )}
    </div>
  );
}

/** Codex(ChatGPT 구독 OAuth) 카드 — auth.json 임포트 + 상태 + 파싱 선택. */
function CodexCard({
  codex,
  isParsing,
  onImport,
  onSetParsing,
  onTest,
}: {
  codex: CodexStatus;
  isParsing: boolean;
  onImport: (authJson: string) => Promise<void>;
  onSetParsing: () => Promise<void>;
  onTest: () => Promise<void>;
}) {
  const [authJson, setAuthJson] = useState('');
  const [isImporting, setIsImporting] = useState(false);
  const [isTesting, setIsTesting] = useState(false);

  const handleImport = async () => {
    if (!authJson.trim()) return;
    setIsImporting(true);
    try {
      await onImport(authJson.trim());
      setAuthJson('');
    } finally {
      setIsImporting(false);
    }
  };

  const handleTest = async () => {
    setIsTesting(true);
    try {
      await onTest();
    } finally {
      setIsTesting(false);
    }
  };

  return (
    <div className="bg-card border border-border rounded-lg p-6">
      <div className="flex justify-between items-start mb-4">
        <div>
          <h3 className="text-lg font-semibold text-gray-200">
            Codex <span className="text-xs text-muted">(ChatGPT 구독 · 파싱 전용)</span>
          </h3>
          <p className="text-sm text-muted">
            {codex.has_auth
              ? `계정: ${codex.account_preview || '••••'}${
                  codex.last_refresh
                    ? ` · 마지막 refresh: ${new Date(codex.last_refresh).toLocaleString()}`
                    : ''
                }`
              : 'auth.json 미임포트'}
          </p>
        </div>
        {isParsing && (
          <span className="text-xs bg-primary/20 text-primary px-2 py-1 rounded">Parsing</span>
        )}
      </div>

      <p className="text-xs text-muted mb-2">
        터미널에서 <code className="text-primary">codex login</code> 후{' '}
        <code className="text-primary">~/.codex/auth.json</code> 내용을 붙여넣으세요.
      </p>
      <textarea
        className="w-full bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 font-mono focus:outline-none focus:border-primary"
        rows={4}
        placeholder={'{ "tokens": { "access_token": "...", "refresh_token": "...", "account_id": "..." } }'}
        value={authJson}
        onChange={(e) => setAuthJson(e.target.value)}
      />
      <div className="flex gap-2 mt-2">
        <button
          className="px-4 py-2 bg-primary text-white text-sm rounded hover:bg-primary-hover transition-colors disabled:opacity-50"
          onClick={handleImport}
          disabled={isImporting || !authJson.trim()}
        >
          {isImporting ? 'Importing...' : 'Import auth.json'}
        </button>
        {codex.has_auth && (
          <button
            className="px-4 py-2 bg-border text-gray-200 text-sm rounded hover:bg-muted transition-colors disabled:opacity-50"
            onClick={handleTest}
            disabled={isTesting}
          >
            {isTesting ? 'Testing...' : 'Test'}
          </button>
        )}
        {codex.has_auth && (
          <button
            className={`px-4 py-2 text-sm rounded transition-colors ${
              isParsing ? 'bg-primary text-white' : 'bg-border text-gray-200 hover:bg-muted'
            }`}
            onClick={onSetParsing}
            disabled={isParsing}
          >
            Use for Parsing
          </button>
        )}
      </div>
    </div>
  );
}

/** 로컬 임베딩 카드 — 모델/차원/캐시 상태 + 임베딩 선택 + 로드 검증. */
function LocalEmbeddingCard({
  local,
  isEmbedding,
  onSetEmbedding,
  onTest,
}: {
  local: LocalStatus;
  isEmbedding: boolean;
  onSetEmbedding: () => Promise<void>;
  onTest: () => Promise<void>;
}) {
  const [isTesting, setIsTesting] = useState(false);
  const handleTest = async () => {
    setIsTesting(true);
    try {
      await onTest();
    } finally {
      setIsTesting(false);
    }
  };

  return (
    <div className="bg-card border border-border rounded-lg p-6">
      <div className="flex justify-between items-start mb-4">
        <div>
          <h3 className="text-lg font-semibold text-gray-200">
            Local Embedding <span className="text-xs text-muted">(외부 키 불요 · 임베딩 전용)</span>
          </h3>
          <p className="text-sm text-muted">
            모델: {local.model} · 차원: {local.dim} · 캐시:{' '}
            {local.cache_ready ? '준비됨' : '미다운로드 (첫 사용 시 자동)'}
          </p>
        </div>
        {isEmbedding && (
          <span className="text-xs bg-success/20 text-success px-2 py-1 rounded">Embedding</span>
        )}
      </div>
      <div className="flex gap-2">
        <button
          className="px-4 py-2 bg-border text-gray-200 text-sm rounded hover:bg-muted transition-colors disabled:opacity-50"
          onClick={handleTest}
          disabled={isTesting}
        >
          {isTesting ? 'Loading model...' : 'Test (load + embed)'}
        </button>
        <button
          className={`px-4 py-2 text-sm rounded transition-colors ${
            isEmbedding ? 'bg-success text-white' : 'bg-border text-gray-200 hover:bg-muted'
          }`}
          onClick={onSetEmbedding}
          disabled={isEmbedding}
        >
          Use for Embedding
        </button>
      </div>
    </div>
  );
}

/** 임베딩 provider별 차원 — 전환 시 reindex 필요 여부 판정용(백엔드 dimension()과 일치). */
const EMBED_DIM: Record<string, number> = { gemini: 3072, openai: 1536, local: 384 };

export function AdminPage({ showToast, onWorkspacesChanged }: AdminPageProps) {
  const [settings, setSettings] = useState<SettingsResponse | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [showReindexModal, setShowReindexModal] = useState(false);
  const [isReindexing, setIsReindexing] = useState(false);
  const [workspaces, setWorkspaces] = useState<WorkspaceSummary[]>([]);
  // 임베딩 provider 전환으로 차원이 바뀌었을 때의 reindex 안내(차원 정보 포함).
  const [reindexNotice, setReindexNotice] = useState<{ from: number; to: number } | null>(null);

  const loadWorkspaces = useCallback(async () => {
    try {
      setWorkspaces(await api.listWorkspaces());
    } catch {
      // 권한 부족 등으로 실패해도 나머지 admin UI는 동작한다.
    }
  }, []);

  useEffect(() => {
    loadSettings();
    loadWorkspaces();
  }, [loadWorkspaces]);

  // 워크스페이스 변경 시 로컬 목록 + 상위(Navbar) 목록을 함께 갱신
  const handleWorkspacesChanged = useCallback(() => {
    loadWorkspaces();
    onWorkspacesChanged();
  }, [loadWorkspaces, onWorkspacesChanged]);

  const loadSettings = async () => {
    try {
      const data = await api.getSettings();
      setSettings(data);
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to load settings', 'error');
    } finally {
      setIsLoading(false);
    }
  };

  const handleSetKey = async (provider: string, apiKey: string) => {
    try {
      await api.setApiKey(provider, apiKey);
      showToast(`API key set for ${provider}`, 'success');
      await loadSettings();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to set API key', 'error');
    }
  };

  const handleDeleteKey = async (provider: string) => {
    if (!confirm(`Delete API key for ${provider}?`)) return;
    try {
      await api.deleteApiKey(provider);
      showToast(`API key deleted for ${provider}`, 'success');
      await loadSettings();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to delete API key', 'error');
    }
  };

  const handleTestKey = async (provider: string) => {
    try {
      const result = await api.testApiKey(provider);
      if (result.valid) {
        showToast(`${provider} API key is valid`, 'success');
      } else {
        showToast(result.message || `${provider} API key is invalid`, 'error');
      }
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Test failed', 'error');
    }
  };

  const handleSetParsing = async (provider: string) => {
    try {
      await api.updateSettings({ parsing_provider: provider });
      showToast(`Parsing provider set to ${provider}`, 'success');
      await loadSettings();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to update', 'error');
    }
  };

  const handleSetEmbedding = async (provider: string) => {
    const prev = settings?.embedding_provider ?? null;
    try {
      await api.updateSettings({ embedding_provider: provider });
      showToast(`Embedding provider set to ${provider}`, 'success');
      // 차원이 바뀌면 검색이 정상 동작하려면 reindex가 필요하다(FR4 명시 에러 회피).
      const prevDim = prev ? EMBED_DIM[prev] : undefined;
      const nextDim = EMBED_DIM[provider];
      if (prevDim !== undefined && nextDim !== undefined && prevDim !== nextDim) {
        setReindexNotice({ from: prevDim, to: nextDim });
      }
      await loadSettings();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to update', 'error');
    }
  };

  const handleImportCodex = async (authJson: string) => {
    try {
      const data = await api.importCodex(authJson);
      setSettings(data);
      showToast('Codex auth.json 임포트 완료', 'success');
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Codex 임포트 실패', 'error');
    }
  };

  const handleReindex = async () => {
    setIsReindexing(true);
    try {
      const result = await api.reindex();
      setShowReindexModal(false);
      setReindexNotice(null); // 마이그레이션 완료 → 안내 해제
      showToast(`Reindex complete: ${result.indexed} documents indexed`, 'success');
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Reindex failed', 'error');
    } finally {
      setIsReindexing(false);
    }
  };

  return (
    <div className="max-w-4xl mx-auto p-6">
      <h2 className="text-xl font-semibold text-gray-200 mb-6">Admin</h2>

      {/* 워크스페이스 · 키 관리 (Qdrant 불가용 시에도 파일 기반으로 동작) */}
      <div className="flex flex-col gap-4 mb-8">
        <WebUiKeySection showToast={showToast} />
        <WorkspacesSection
          showToast={showToast}
          workspaces={workspaces}
          onChanged={handleWorkspacesChanged}
        />
        <ConnectorsSection showToast={showToast} workspaces={workspaces} />
        <ApiKeysSection showToast={showToast} workspaces={workspaces} />
      </div>

      <h2 className="text-xl font-semibold text-gray-200 mb-6">Model Settings</h2>

      {isLoading ? (
        <div className="text-center text-muted py-8">Loading...</div>
      ) : settings ? (
        <div className="flex flex-col gap-4">
          {settings.providers.map((provider) => (
            <ProviderCard
              key={provider.provider}
              provider={provider}
              parsingProvider={settings.parsing_provider}
              embeddingProvider={settings.embedding_provider}
              onSetKey={handleSetKey}
              onDeleteKey={handleDeleteKey}
              onTestKey={handleTestKey}
              onSetParsing={handleSetParsing}
              onSetEmbedding={handleSetEmbedding}
            />
          ))}

          {/* Codex (구독 파싱) · Local (임베딩) — 키가 아닌 별도 활성화 경로 */}
          <CodexCard
            codex={settings.codex}
            isParsing={settings.parsing_provider === 'codex'}
            onImport={handleImportCodex}
            onSetParsing={() => handleSetParsing('codex')}
            onTest={() => handleTestKey('codex')}
          />
          <LocalEmbeddingCard
            local={settings.local}
            isEmbedding={settings.embedding_provider === 'local'}
            onSetEmbedding={() => handleSetEmbedding('local')}
            onTest={() => handleTestKey('local')}
          />

          {/* 차원 변경 감지 시 reindex 경고 (전환 후 검색이 "reindex 필요" 에러를 내기 전 안내) */}
          {reindexNotice && (
            <div className="bg-warning/10 border border-warning rounded-lg p-6">
              <h3 className="text-lg font-semibold text-warning mb-2">⚠ 차원 변경 — reindex 필요</h3>
              <p className="text-sm text-gray-200 mb-4">
                임베딩 차원이 {reindexNotice.from} → {reindexNotice.to}로 바뀌었습니다. 검색이 정상
                동작하려면 저장된 문서를 새 차원으로 재인덱싱해야 합니다(문서 손실 없음).
              </p>
              <button
                className="px-4 py-2 bg-warning text-black rounded-lg hover:opacity-90 transition-opacity"
                onClick={() => setShowReindexModal(true)}
              >
                지금 Reindex
              </button>
            </div>
          )}

          {/* Maintenance */}
          <div className="bg-card border border-border rounded-lg p-6 mt-4">
            <h3 className="text-lg font-semibold text-gray-200 mb-2">Maintenance</h3>
            <p className="text-sm text-muted mb-4">
              Rebuild the vector search index from stored documents. Use this after
              changing the embedding provider or if search results seem incorrect.
            </p>
            <button
              className="px-4 py-2 bg-primary text-white rounded-lg hover:bg-primary-hover transition-colors"
              onClick={() => setShowReindexModal(true)}
            >
              Reindex All Documents
            </button>
          </div>
        </div>
      ) : (
        <div className="text-center text-muted py-8">Failed to load settings</div>
      )}

      {showReindexModal && (
        <ConfirmModal
          title="Reindex All Documents"
          description={
            'All stored documents will be re-embedded and re-indexed into the vector database.\n\n' +
            'This may take a while depending on the number of documents and will consume embedding API credits.'
          }
          confirmLabel="Reindex"
          isLoading={isReindexing}
          onConfirm={handleReindex}
          onCancel={() => !isReindexing && setShowReindexModal(false)}
        />
      )}
    </div>
  );
}
