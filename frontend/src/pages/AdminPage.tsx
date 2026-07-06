import { useState, useEffect, useCallback } from 'react';
import type { ProviderInfo, SettingsResponse, WorkspaceSummary } from '../api/types';
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
        </div>
      )}
    </div>
  );
}

export function AdminPage({ showToast, onWorkspacesChanged }: AdminPageProps) {
  const [settings, setSettings] = useState<SettingsResponse | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [showReindexModal, setShowReindexModal] = useState(false);
  const [isReindexing, setIsReindexing] = useState(false);
  const [workspaces, setWorkspaces] = useState<WorkspaceSummary[]>([]);

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
    try {
      await api.updateSettings({ embedding_provider: provider });
      showToast(`Embedding provider set to ${provider}`, 'success');
      await loadSettings();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to update', 'error');
    }
  };

  const handleReindex = async () => {
    setIsReindexing(true);
    try {
      const result = await api.reindex();
      setShowReindexModal(false);
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
