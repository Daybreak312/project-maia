import { useState, useEffect, useCallback } from 'react';
import { api, getAuthKey, setAuthKey } from '../api/client';
import type {
  WorkspaceSummary,
  WorkspaceTemplate,
  ApiKeyInfo,
  Permission,
  CreateKeyResponse,
} from '../api/types';

type Toast = (message: string, type: 'success' | 'error') => void;

// ─────────────────────────────────────────────────────────────────────
// Web UI 인증 키 — admin 작업에 사용할 Bearer 토큰 (localStorage 영속)
// ─────────────────────────────────────────────────────────────────────
export function WebUiKeySection({ showToast }: { showToast: Toast }) {
  const [key, setKey] = useState(getAuthKey());
  const [editing, setEditing] = useState(false);

  const save = () => {
    setAuthKey(key.trim());
    setEditing(false);
    showToast('Web UI key saved. Reload to apply to all requests.', 'success');
  };

  const masked = getAuthKey() ? '••••••••' + getAuthKey().slice(-4) : 'not set';

  return (
    <div className="bg-card border border-border rounded-lg p-6">
      <h3 className="text-lg font-semibold text-gray-200 mb-2">Web UI Authentication</h3>
      <p className="text-sm text-muted mb-4">
        API key used by this web UI for authenticated requests (master key or an admin key).
        Stored locally in your browser. Current: <span className="font-mono">{masked}</span>
      </p>
      {editing ? (
        <div className="flex gap-2">
          <input
            type="password"
            className="flex-1 bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
            placeholder="Paste API key..."
            value={key}
            onChange={(e) => setKey(e.target.value)}
          />
          <button
            className="px-4 py-2 bg-primary text-white text-sm rounded hover:bg-primary-hover transition-colors"
            onClick={save}
          >
            Save
          </button>
          <button
            className="px-4 py-2 bg-border text-gray-200 text-sm rounded hover:bg-muted transition-colors"
            onClick={() => {
              setKey(getAuthKey());
              setEditing(false);
            }}
          >
            Cancel
          </button>
        </div>
      ) : (
        <div className="flex gap-2">
          <button
            className="px-4 py-2 bg-border text-gray-200 text-sm rounded hover:bg-muted transition-colors"
            onClick={() => setEditing(true)}
          >
            {getAuthKey() ? 'Update Key' : 'Set Key'}
          </button>
          {getAuthKey() && (
            <button
              className="px-4 py-2 bg-error text-white text-sm rounded hover:bg-red-700 transition-colors"
              onClick={() => {
                setAuthKey('');
                setKey('');
                showToast('Web UI key cleared', 'success');
              }}
            >
              Clear
            </button>
          )}
        </div>
      )}
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────
// 워크스페이스 관리 (목록 / 생성 / 삭제)
// ─────────────────────────────────────────────────────────────────────
export function WorkspacesSection({
  showToast,
  workspaces,
  onChanged,
}: {
  showToast: Toast;
  workspaces: WorkspaceSummary[];
  onChanged: () => void;
}) {
  const [newId, setNewId] = useState('');
  const [newName, setNewName] = useState('');
  const [template, setTemplate] = useState<WorkspaceTemplate>('personal');
  const [creating, setCreating] = useState(false);

  const create = async () => {
    if (!newId.trim() || !newName.trim()) return;
    setCreating(true);
    try {
      await api.createWorkspace({ id: newId.trim(), name: newName.trim(), template });
      showToast(`Workspace '${newId.trim()}' created`, 'success');
      setNewId('');
      setNewName('');
      onChanged();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to create workspace', 'error');
    } finally {
      setCreating(false);
    }
  };

  const remove = async (id: string) => {
    if (!confirm(`Delete workspace '${id}'? All its documents and vectors will be removed.`)) return;
    try {
      await api.deleteWorkspace(id);
      showToast(`Workspace '${id}' deleted`, 'success');
      onChanged();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to delete workspace', 'error');
    }
  };

  return (
    <div className="bg-card border border-border rounded-lg p-6">
      <h3 className="text-lg font-semibold text-gray-200 mb-4">Workspaces</h3>

      <div className="flex flex-col gap-2 mb-4">
        {workspaces.length === 0 && (
          <p className="text-sm text-muted">No workspaces (or insufficient permission to list).</p>
        )}
        {workspaces.map((ws) => (
          <div
            key={ws.id}
            className="flex justify-between items-center bg-bg border border-border rounded px-3 py-2"
          >
            <div className="text-sm">
              <span className="text-gray-200 font-medium">{ws.name}</span>{' '}
              <span className="text-muted font-mono">({ws.id})</span>{' '}
              <span className="text-xs text-muted">· {ws.template}</span>
              {ws.search?.cross_workspace?.length > 0 && (
                <span className="text-xs text-muted"> · cross: {ws.search.cross_workspace.join(', ')}</span>
              )}
            </div>
            {ws.id !== 'default' && (
              <button
                className="px-3 py-1 bg-error text-white text-xs rounded hover:bg-red-700 transition-colors"
                onClick={() => remove(ws.id)}
              >
                Delete
              </button>
            )}
          </div>
        ))}
      </div>

      <div className="flex flex-wrap gap-2 items-center border-t border-border pt-4">
        <input
          className="w-32 bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
          placeholder="id (e.g. work)"
          value={newId}
          onChange={(e) => setNewId(e.target.value)}
        />
        <input
          className="w-40 bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
          placeholder="Display name"
          value={newName}
          onChange={(e) => setNewName(e.target.value)}
        />
        <select
          className="bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
          value={template}
          onChange={(e) => setTemplate(e.target.value as WorkspaceTemplate)}
        >
          <option value="personal">personal</option>
          <option value="enterprise">enterprise</option>
        </select>
        <button
          className="px-4 py-2 bg-primary text-white text-sm rounded hover:bg-primary-hover transition-colors disabled:opacity-50"
          onClick={create}
          disabled={creating || !newId.trim() || !newName.trim()}
        >
          {creating ? 'Creating...' : 'Create'}
        </button>
      </div>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────
// API 키 관리 (목록 / 발급 / 폐기)
// ─────────────────────────────────────────────────────────────────────
export function ApiKeysSection({
  showToast,
  workspaces,
}: {
  showToast: Toast;
  workspaces: WorkspaceSummary[];
}) {
  const [keys, setKeys] = useState<ApiKeyInfo[]>([]);
  const [label, setLabel] = useState('');
  const [permission, setPermission] = useState<Permission>('read_write');
  const [selectedWs, setSelectedWs] = useState<string[]>([]);
  const [creating, setCreating] = useState(false);
  const [issuedKey, setIssuedKey] = useState<CreateKeyResponse | null>(null);

  const load = useCallback(async () => {
    try {
      setKeys(await api.listKeys());
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to load API keys', 'error');
    }
  }, [showToast]);

  useEffect(() => {
    load();
  }, [load]);

  const toggleWs = (id: string) => {
    setSelectedWs((prev) =>
      prev.includes(id) ? prev.filter((w) => w !== id) : [...prev, id],
    );
  };

  const create = async () => {
    if (!label.trim()) return;
    setCreating(true);
    try {
      const res = await api.createKey({
        label: label.trim(),
        workspaces: selectedWs,
        permissions: permission,
      });
      setIssuedKey(res);
      showToast(`Key '${label.trim()}' issued`, 'success');
      setLabel('');
      setSelectedWs([]);
      await load();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to issue key', 'error');
    } finally {
      setCreating(false);
    }
  };

  const revoke = async (keyId: string, keyLabel: string) => {
    if (!confirm(`Revoke key '${keyLabel}' (${keyId})?`)) return;
    try {
      await api.revokeKey(keyId);
      showToast('Key revoked', 'success');
      await load();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to revoke key', 'error');
    }
  };

  return (
    <div className="bg-card border border-border rounded-lg p-6">
      <h3 className="text-lg font-semibold text-gray-200 mb-4">API Keys</h3>

      {/* 방금 발급된 평문 키 — 1회성 표시 */}
      {issuedKey && (
        <div className="bg-primary/10 border border-primary rounded p-4 mb-4">
          <p className="text-sm text-gray-200 mb-2">
            Copy this key now — it will not be shown again:
          </p>
          <code className="block bg-bg border border-border rounded px-3 py-2 text-sm text-primary break-all">
            {issuedKey.api_key}
          </code>
          <button
            className="mt-2 px-3 py-1 bg-border text-gray-200 text-xs rounded hover:bg-muted transition-colors"
            onClick={() => setIssuedKey(null)}
          >
            Dismiss
          </button>
        </div>
      )}

      <div className="flex flex-col gap-2 mb-4">
        {keys.length === 0 && (
          <p className="text-sm text-muted">No API keys issued yet.</p>
        )}
        {keys.map((k) => (
          <div
            key={k.key_id}
            className="flex justify-between items-center bg-bg border border-border rounded px-3 py-2"
          >
            <div className="text-sm">
              <span className="text-gray-200 font-medium">{k.label}</span>{' '}
              <span className="text-xs text-muted font-mono">({k.key_id})</span>
              <div className="text-xs text-muted">
                {k.permissions} · ws: {k.workspaces.length ? k.workspaces.join(', ') : 'all'} ·
                last used: {k.last_used_at ? new Date(k.last_used_at).toLocaleString() : 'never'}
              </div>
            </div>
            <button
              className="px-3 py-1 bg-error text-white text-xs rounded hover:bg-red-700 transition-colors"
              onClick={() => revoke(k.key_id, k.label)}
            >
              Revoke
            </button>
          </div>
        ))}
      </div>

      {/* 발급 폼 */}
      <div className="border-t border-border pt-4 flex flex-col gap-3">
        <div className="flex flex-wrap gap-2 items-center">
          <input
            className="w-48 bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
            placeholder="Label (e.g. iPad)"
            value={label}
            onChange={(e) => setLabel(e.target.value)}
          />
          <select
            className="bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
            value={permission}
            onChange={(e) => setPermission(e.target.value as Permission)}
          >
            <option value="read_only">read_only</option>
            <option value="read_write">read_write</option>
            <option value="admin">admin</option>
          </select>
          <button
            className="px-4 py-2 bg-primary text-white text-sm rounded hover:bg-primary-hover transition-colors disabled:opacity-50"
            onClick={create}
            disabled={creating || !label.trim()}
          >
            {creating ? 'Issuing...' : 'Issue Key'}
          </button>
        </div>
        {workspaces.length > 0 && (
          <div className="flex flex-wrap gap-3 text-sm text-muted">
            <span>Scope:</span>
            {workspaces.map((ws) => (
              <label key={ws.id} className="flex items-center gap-1 cursor-pointer">
                <input
                  type="checkbox"
                  checked={selectedWs.includes(ws.id)}
                  onChange={() => toggleWs(ws.id)}
                />
                {ws.id}
              </label>
            ))}
            <span className="text-xs">(none selected = all workspaces)</span>
          </div>
        )}
      </div>
    </div>
  );
}
