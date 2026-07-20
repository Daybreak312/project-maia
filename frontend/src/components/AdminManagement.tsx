import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { api, getAuthKey, setAuthKey } from '../api/client';
import type {
  WorkspaceSummary,
  WorkspaceTemplate,
  ApiKeyInfo,
  Permission,
  CreateKeyResponse,
  ConnectorView,
  UserInfo,
  MeResponse,
  MemberView,
  MembersResponse,
  WorkspaceVisibility,
} from '../api/types';

type Toast = (message: string, type: 'success' | 'error') => void;

/** 백엔드 검증 규칙과 동일 (auth/users.rs MIN_PASSWORD_LEN) — 사전 안내용. */
const MIN_PASSWORD_LEN = 8;

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
    // 영속 키는 반드시 1개 이상의 워크스페이스로 스코프되어야 한다 (fail-closed).
    // 스코프 없는 키는 백엔드가 400으로 거부한다.
    if (!label.trim() || selectedWs.length === 0) return;
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
                {k.permissions} · ws: {k.workspaces.length ? k.workspaces.join(', ') : 'none'} ·
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
            disabled={creating || !label.trim() || selectedWs.length === 0}
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
            <span className="text-xs">(select at least one workspace — required)</span>
          </div>
        )}
        {workspaces.length === 0 && (
          <p className="text-xs text-muted">
            No workspaces available to scope — create a workspace first (or check your permissions).
          </p>
        )}
      </div>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────
// 커넥터 관리 (Phase 4) — 로컬 디렉토리 유입 파이프라인 등록/상태/즉시 실행
// ─────────────────────────────────────────────────────────────────────
function formatSummary(view: ConnectorView): string {
  const r = view.state.last_result;
  if (view.progress?.running) {
    return `running… ${view.progress.processed}/${view.progress.total} (new ${view.progress.created}, upd ${view.progress.updated}, fail ${view.progress.failed})`;
  }
  if (!r) return 'never synced';
  const when = view.state.last_run_at
    ? new Date(view.state.last_run_at).toLocaleString()
    : 'unknown';
  return `${when} · processed ${r.processed}, new ${r.created}, upd ${r.updated}, skip ${r.skipped}, fail ${r.failed}`;
}

export function ConnectorsSection({
  showToast,
  workspaces,
}: {
  showToast: Toast;
  workspaces: WorkspaceSummary[];
}) {
  const [selectedWs, setSelectedWs] = useState('default');
  const [connectors, setConnectors] = useState<ConnectorView[]>([]);
  // 등록 폼
  const [newId, setNewId] = useState('');
  const [dirs, setDirs] = useState('');
  const [extensions, setExtensions] = useState('md, markdown, txt');
  const [intervalMins, setIntervalMins] = useState(60);
  const [creating, setCreating] = useState(false);

  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const load = useCallback(async () => {
    try {
      setConnectors(await api.listConnectors(selectedWs));
    } catch {
      // 권한 부족·워크스페이스 없음 등은 조용히 빈 목록.
      setConnectors([]);
    }
  }, [selectedWs]);

  // 선택 워크스페이스가 바뀌면 다시 로드하고, 5초 주기로 상태를 폴링한다(진행 관측).
  useEffect(() => {
    load();
    pollRef.current = setInterval(load, 5000);
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [load]);

  const register = async () => {
    const directories = dirs.split(',').map((s) => s.trim()).filter(Boolean);
    if (!newId.trim() || directories.length === 0) return;
    setCreating(true);
    try {
      await api.registerConnector(selectedWs, {
        id: newId.trim(),
        interval_secs: Math.max(1, intervalMins) * 60,
        spec: {
          type: 'local_directory',
          directories,
          extensions: extensions.split(',').map((s) => s.trim()).filter(Boolean),
          exclude: [],
          max_file_bytes: 1_048_576,
        },
      });
      showToast(`Connector '${newId.trim()}' registered`, 'success');
      setNewId('');
      setDirs('');
      await load();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to register connector', 'error');
    } finally {
      setCreating(false);
    }
  };

  const triggerSync = async (id: string, full: boolean, mode: 'parsed' | 'raw') => {
    try {
      await api.triggerConnectorSync(selectedWs, id, { full, mode });
      showToast(`Sync started for '${id}'${full ? ' (full)' : ''}`, 'success');
      // 잠시 뒤 진행이 반영되도록 즉시 한 번 새로고침.
      setTimeout(load, 500);
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to start sync', 'error');
    }
  };

  const remove = async (id: string) => {
    if (!confirm(`Delete connector '${id}'? Ingested documents are kept; only the source link is removed.`))
      return;
    try {
      await api.deleteConnector(selectedWs, id);
      showToast(`Connector '${id}' deleted`, 'success');
      await load();
    } catch (err) {
      showToast(err instanceof Error ? err.message : 'Failed to delete connector', 'error');
    }
  };

  return (
    <div className="bg-card border border-border rounded-lg p-6">
      <div className="flex justify-between items-center mb-4">
        <h3 className="text-lg font-semibold text-gray-200">Connectors</h3>
        <select
          className="bg-bg border border-border rounded px-3 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-primary"
          value={selectedWs}
          onChange={(e) => setSelectedWs(e.target.value)}
        >
          {workspaces.length === 0 && <option value="default">default</option>}
          {workspaces.map((ws) => (
            <option key={ws.id} value={ws.id}>
              {ws.id}
            </option>
          ))}
        </select>
      </div>

      <div className="flex flex-col gap-2 mb-4">
        {connectors.length === 0 && (
          <p className="text-sm text-muted">No connectors registered in this workspace.</p>
        )}
        {connectors.map((c) => (
          <div key={c.instance.id} className="bg-bg border border-border rounded px-3 py-2">
            <div className="flex justify-between items-start gap-2">
              <div className="text-sm min-w-0">
                <span className="text-gray-200 font-medium">{c.instance.id}</span>{' '}
                <span className="text-xs text-muted">· {c.instance.spec.type}</span>{' '}
                <span className="text-xs text-muted">
                  · every {Math.round(c.instance.interval_secs / 60)}m
                </span>{' '}
                {!c.instance.enabled && (
                  <span className="text-xs text-error">· disabled</span>
                )}
                <div className="text-xs text-muted truncate">
                  dirs: {c.instance.spec.directories.join(', ')}
                </div>
                <div className="text-xs text-muted">{formatSummary(c)}</div>
                {c.state.last_result && c.state.last_result.failures.length > 0 && (
                  <div className="text-xs text-error">
                    failures: {c.state.last_result.failures.length} (e.g.{' '}
                    {c.state.last_result.failures[0].source_id})
                  </div>
                )}
              </div>
              <div className="flex flex-col gap-1 shrink-0">
                <button
                  className="px-3 py-1 bg-primary text-white text-xs rounded hover:bg-primary-hover transition-colors disabled:opacity-50"
                  onClick={() => triggerSync(c.instance.id, false, 'parsed')}
                  disabled={c.progress?.running}
                >
                  Sync
                </button>
                <button
                  className="px-3 py-1 bg-border text-gray-200 text-xs rounded hover:bg-muted transition-colors disabled:opacity-50"
                  onClick={() => triggerSync(c.instance.id, true, 'parsed')}
                  disabled={c.progress?.running}
                  title="Ignore cursor and rescan all files"
                >
                  Full
                </button>
                <button
                  className="px-3 py-1 bg-error text-white text-xs rounded hover:bg-red-700 transition-colors"
                  onClick={() => remove(c.instance.id)}
                >
                  Delete
                </button>
              </div>
            </div>
          </div>
        ))}
      </div>

      {/* 등록 폼 (로컬 디렉토리) */}
      <div className="border-t border-border pt-4 flex flex-col gap-2">
        <div className="flex flex-wrap gap-2 items-center">
          <input
            className="w-32 bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
            placeholder="id (e.g. notes)"
            value={newId}
            onChange={(e) => setNewId(e.target.value)}
          />
          <input
            className="flex-1 min-w-[16rem] bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
            placeholder="directories (comma-separated absolute paths)"
            value={dirs}
            onChange={(e) => setDirs(e.target.value)}
          />
        </div>
        <div className="flex flex-wrap gap-2 items-center">
          <input
            className="w-48 bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
            placeholder="extensions"
            value={extensions}
            onChange={(e) => setExtensions(e.target.value)}
          />
          <label className="text-xs text-muted flex items-center gap-1">
            interval (min)
            <input
              type="number"
              min={1}
              className="w-20 bg-bg border border-border rounded px-2 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
              value={intervalMins}
              onChange={(e) => setIntervalMins(Number(e.target.value) || 1)}
            />
          </label>
          <button
            className="px-4 py-2 bg-primary text-white text-sm rounded hover:bg-primary-hover transition-colors disabled:opacity-50"
            onClick={register}
            disabled={creating || !newId.trim() || !dirs.trim()}
          >
            {creating ? 'Registering...' : 'Register'}
          </button>
        </div>
        <p className="text-xs text-muted">
          Local directory connector: scans registered dirs for changed markdown/text and ingests
          them (incremental, de-duplicated by source path).
        </p>
      </div>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────
// 계정 관리 (글로벌 admin 전용) — 목록 / 생성 / 삭제 / 비밀번호 변경
// ─────────────────────────────────────────────────────────────────────
function UserRow({
  user,
  isSelf,
  showToast,
  onDeleted,
}: {
  user: UserInfo;
  isSelf: boolean;
  showToast: Toast;
  onDeleted: () => void;
}) {
  const [showPasswordForm, setShowPasswordForm] = useState(false);
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);

  const changePassword = async () => {
    if (password.length < MIN_PASSWORD_LEN) return;
    setBusy(true);
    try {
      await api.changePassword(user.user_id, password);
      showToast(`'${user.username}'의 비밀번호가 변경되었습니다`, 'success');
      setPassword('');
      setShowPasswordForm(false);
    } catch (err) {
      showToast(err instanceof Error ? err.message : '비밀번호 변경 실패', 'error');
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (
      !confirm(
        `계정 '${user.username}'을(를) 삭제할까요? 소유 API 키·세션·모든 워크스페이스 멤버십이 함께 제거됩니다 (개인 워크스페이스 데이터는 보존됩니다).`,
      )
    )
      return;
    try {
      await api.deleteUser(user.user_id);
      showToast(`계정 '${user.username}'이(가) 삭제되었습니다`, 'success');
      onDeleted();
    } catch (err) {
      showToast(err instanceof Error ? err.message : '계정 삭제 실패', 'error');
    }
  };

  return (
    <div className="bg-bg border border-border rounded px-3 py-2">
      <div className="flex justify-between items-center gap-2">
        <div className="text-sm min-w-0">
          <span className="text-gray-200 font-medium">{user.display_name}</span>{' '}
          <span className="text-muted font-mono">({user.username})</span>{' '}
          {user.is_admin && (
            <span className="text-xs bg-primary/20 text-primary px-2 py-0.5 rounded ml-1">
              admin
            </span>
          )}
          {isSelf && (
            <span className="text-xs bg-border text-muted px-2 py-0.5 rounded ml-1">나</span>
          )}
          <div className="text-xs text-muted font-mono">{user.user_id}</div>
        </div>
        <div className="flex gap-2 shrink-0">
          <button
            className="px-3 py-1 bg-border text-gray-200 text-xs rounded hover:bg-muted transition-colors"
            onClick={() => setShowPasswordForm((v) => !v)}
          >
            비밀번호 변경
          </button>
          {!isSelf && (
            <button
              className="px-3 py-1 bg-error text-white text-xs rounded hover:bg-red-700 transition-colors"
              onClick={remove}
            >
              삭제
            </button>
          )}
        </div>
      </div>
      {showPasswordForm && (
        <div className="flex gap-2 mt-2 pt-2 border-t border-border">
          <input
            type="password"
            className="flex-1 bg-card border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
            placeholder={`새 비밀번호 (${MIN_PASSWORD_LEN}자 이상)`}
            autoComplete="new-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
          <button
            className="px-4 py-2 bg-primary text-white text-sm rounded hover:bg-primary-hover transition-colors disabled:opacity-50"
            onClick={changePassword}
            disabled={busy || password.length < MIN_PASSWORD_LEN}
          >
            {busy ? '변경 중...' : '변경'}
          </button>
        </div>
      )}
    </div>
  );
}

export function UsersSection({
  showToast,
  currentUserId,
  onWorkspacesChanged,
}: {
  showToast: Toast;
  currentUserId: string | undefined;
  /** 계정 생성은 개인 워크스페이스를 함께 만든다 — 상위(Workspaces 등) 목록도 갱신한다. */
  onWorkspacesChanged: () => void;
}) {
  const [users, setUsers] = useState<UserInfo[]>([]);
  const [username, setUsername] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [password, setPassword] = useState('');
  const [isAdmin, setIsAdmin] = useState(false);
  const [creating, setCreating] = useState(false);

  const load = useCallback(async () => {
    try {
      setUsers(await api.listUsers());
    } catch (err) {
      showToast(err instanceof Error ? err.message : '계정 목록 조회 실패', 'error');
    }
  }, [showToast]);

  useEffect(() => {
    load();
  }, [load]);

  const create = async () => {
    if (!username.trim() || password.length < MIN_PASSWORD_LEN) return;
    setCreating(true);
    try {
      const res = await api.createUser({
        username: username.trim(),
        password,
        display_name: displayName.trim() || undefined,
        is_admin: isAdmin,
      });
      showToast(
        `계정 '${res.user.username}'이(가) 생성되었습니다 (개인 워크스페이스: ${res.personal_workspace})`,
        'success',
      );
      setUsername('');
      setDisplayName('');
      setPassword('');
      setIsAdmin(false);
      await load();
      onWorkspacesChanged();
    } catch (err) {
      showToast(err instanceof Error ? err.message : '계정 생성 실패', 'error');
    } finally {
      setCreating(false);
    }
  };

  return (
    <div className="bg-card border border-border rounded-lg p-6">
      <h3 className="text-lg font-semibold text-gray-200 mb-4">계정 관리</h3>

      <div className="flex flex-col gap-2 mb-4">
        {users.length === 0 && <p className="text-sm text-muted">등록된 계정이 없습니다.</p>}
        {users.map((u) => (
          <UserRow
            key={u.user_id}
            user={u}
            isSelf={u.user_id === currentUserId}
            showToast={showToast}
            onDeleted={load}
          />
        ))}
      </div>

      <div className="flex flex-wrap gap-2 items-center border-t border-border pt-4">
        <input
          className="w-32 bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
          placeholder="아이디"
          value={username}
          onChange={(e) => setUsername(e.target.value)}
        />
        <input
          className="w-40 bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
          placeholder="표시 이름 (선택)"
          value={displayName}
          onChange={(e) => setDisplayName(e.target.value)}
        />
        <input
          type="password"
          className="w-44 bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
          placeholder={`초기 비밀번호 (${MIN_PASSWORD_LEN}자 이상)`}
          autoComplete="new-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        <label className="flex items-center gap-1 text-sm text-muted cursor-pointer">
          <input
            type="checkbox"
            checked={isAdmin}
            onChange={(e) => setIsAdmin(e.target.checked)}
          />
          admin
        </label>
        <button
          className="px-4 py-2 bg-primary text-white text-sm rounded hover:bg-primary-hover transition-colors disabled:opacity-50"
          onClick={create}
          disabled={creating || !username.trim() || password.length < MIN_PASSWORD_LEN}
        >
          {creating ? '생성 중...' : '계정 생성'}
        </button>
      </div>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────
// 워크스페이스 멤버십 · 공개 설정 (글로벌 admin 또는 해당 워크스페이스 role=admin)
// ─────────────────────────────────────────────────────────────────────
const ROLE_OPTIONS: Permission[] = ['read_only', 'read_write', 'admin'];
// public_permission은 admin을 지정할 수 없다 (백엔드가 400으로 거부).
const PUBLIC_PERMISSION_OPTIONS: Permission[] = ['read_only', 'read_write'];

function MemberRow({
  member,
  workspaceId,
  showToast,
  onChanged,
}: {
  member: MemberView;
  workspaceId: string;
  showToast: Toast;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);

  const changeRole = async (role: Permission) => {
    setBusy(true);
    try {
      await api.upsertMember(workspaceId, member.user_id, role);
      showToast('역할이 변경되었습니다', 'success');
      await onChanged();
    } catch (err) {
      showToast(err instanceof Error ? err.message : '역할 변경 실패', 'error');
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!confirm(`'${member.username ?? member.user_id}'을(를) 이 워크스페이스에서 제거할까요?`))
      return;
    setBusy(true);
    try {
      await api.removeMember(workspaceId, member.user_id);
      showToast('멤버가 제거되었습니다', 'success');
      await onChanged();
    } catch (err) {
      showToast(err instanceof Error ? err.message : '멤버 제거 실패', 'error');
      setBusy(false);
    }
  };

  return (
    <div className="flex justify-between items-center bg-bg border border-border rounded px-3 py-2">
      <div className="text-sm">
        {member.username ? (
          <>
            <span className="text-gray-200 font-medium">{member.display_name}</span>{' '}
            <span className="text-muted font-mono">({member.username})</span>
          </>
        ) : (
          <span className="text-muted italic">(삭제된 계정 · {member.user_id})</span>
        )}
      </div>
      <div className="flex gap-2 items-center">
        <select
          className="bg-card border border-border rounded px-2 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-primary disabled:opacity-50"
          value={member.role}
          onChange={(e) => changeRole(e.target.value as Permission)}
          disabled={busy}
        >
          {ROLE_OPTIONS.map((r) => (
            <option key={r} value={r}>
              {r}
            </option>
          ))}
        </select>
        <button
          className="px-3 py-1 bg-error text-white text-xs rounded hover:bg-red-700 transition-colors disabled:opacity-50"
          onClick={remove}
          disabled={busy}
        >
          제거
        </button>
      </div>
    </div>
  );
}

/** 멤버 추가 폼. 계정 목록 조회 권한(글로벌 admin)이 있으면 검색 드롭다운을,
 * 없으면(워크스페이스-레벨 admin) user_id 직접 입력을 제공한다. */
function AddMemberForm({
  workspaceId,
  existingMemberIds,
  allUsers,
  showToast,
  onAdded,
}: {
  workspaceId: string;
  existingMemberIds: Set<string>;
  allUsers: UserInfo[] | null;
  showToast: Toast;
  onAdded: () => void;
}) {
  const candidates = allUsers?.filter((u) => !existingMemberIds.has(u.user_id)) ?? [];
  const [selectedUserId, setSelectedUserId] = useState('');
  const [manualUserId, setManualUserId] = useState('');
  const [role, setRole] = useState<Permission>('read_write');
  const [busy, setBusy] = useState(false);

  const targetUserId = allUsers !== null ? selectedUserId : manualUserId.trim();

  const add = async () => {
    if (!targetUserId) return;
    setBusy(true);
    try {
      await api.upsertMember(workspaceId, targetUserId, role);
      showToast('멤버가 추가되었습니다', 'success');
      setSelectedUserId('');
      setManualUserId('');
      await onAdded();
    } catch (err) {
      showToast(err instanceof Error ? err.message : '멤버 추가 실패', 'error');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex flex-wrap gap-2 items-center">
      {allUsers !== null ? (
        <select
          className="bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
          value={selectedUserId}
          onChange={(e) => setSelectedUserId(e.target.value)}
        >
          <option value="">계정 선택...</option>
          {candidates.map((u) => (
            <option key={u.user_id} value={u.user_id}>
              {u.display_name} ({u.username})
            </option>
          ))}
        </select>
      ) : (
        <input
          className="w-56 bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 font-mono focus:outline-none focus:border-primary"
          placeholder="user_id 직접 입력"
          value={manualUserId}
          onChange={(e) => setManualUserId(e.target.value)}
        />
      )}
      <select
        className="bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
        value={role}
        onChange={(e) => setRole(e.target.value as Permission)}
      >
        {ROLE_OPTIONS.map((r) => (
          <option key={r} value={r}>
            {r}
          </option>
        ))}
      </select>
      <button
        className="px-4 py-2 bg-primary text-white text-sm rounded hover:bg-primary-hover transition-colors disabled:opacity-50"
        onClick={add}
        disabled={busy || !targetUserId}
      >
        {busy ? '추가 중...' : '멤버 추가'}
      </button>
      {allUsers === null && (
        <span className="text-xs text-muted">
          (계정 목록 조회 권한이 없어 user_id를 직접 입력해야 합니다 — 대상 계정의 "내 계정"
          페이지에서 확인할 수 있습니다)
        </span>
      )}
    </div>
  );
}

export function MembersSection({
  showToast,
  me,
  workspaces,
}: {
  showToast: Toast;
  me: MeResponse;
  workspaces: WorkspaceSummary[];
}) {
  // 관리 가능한 워크스페이스 = 글로벌 admin이 볼 수 있는 전체 목록(이름 포함) ∪
  // 워크스페이스-레벨 admin으로 소속된 항목(이름 미상 — id만). 글로벌 admin이
  // 아니면 `workspaces`는 항상 비어있으므로(403) 자연히 후자만 남는다.
  const options = useMemo(() => {
    const byId = new Map<string, string | null>();
    workspaces.forEach((w) => byId.set(w.id, w.name));
    me.workspaces
      .filter((w) => w.permission === 'admin')
      .forEach((w) => {
        if (!byId.has(w.id)) byId.set(w.id, null);
      });
    return Array.from(byId.entries()).map(([id, name]) => ({ id, name }));
  }, [workspaces, me.workspaces]);

  const [selectedWs, setSelectedWs] = useState('');
  const [data, setData] = useState<MembersResponse | null>(null);
  const [allUsers, setAllUsers] = useState<UserInfo[] | null>(null);
  const [changingVisibility, setChangingVisibility] = useState(false);

  // selectedWs는 "사용자가 드롭다운에서 고른 값"의 의도만 담는다. 실제로 쓰는
  // 워크스페이스는 렌더 중 즉시 파생한다 — 옵션이 바뀌어 선택값이 무효해지는
  // 경우(초기 로드·워크스페이스 삭제 등)를 effect로 동기화하면 selectedWs
  // 갱신이 load-effect의 재실행을 유발하는 체이닝(cascading render)이 생긴다.
  const activeWs = options.some((o) => o.id === selectedWs) ? selectedWs : (options[0]?.id ?? '');

  // 계정 검색 드롭다운은 글로벌 admin만 (워크스페이스-레벨 admin은 /api/users가 403).
  useEffect(() => {
    if (!me.is_admin) return;
    api
      .listUsers()
      .then(setAllUsers)
      .catch(() => setAllUsers(null));
  }, [me.is_admin]);

  const load = useCallback(async () => {
    // activeWs가 빈 값이면(options.length === 0) JSX가 "관리 권한 없음" 분기를
    // 먼저 렌더링하므로 data는 참조되지 않는다 — 굳이 정리할 필요가 없다.
    if (!activeWs) return;
    try {
      setData(await api.listMembers(activeWs));
    } catch (err) {
      showToast(err instanceof Error ? err.message : '멤버 목록 조회 실패', 'error');
      setData(null);
    }
  }, [activeWs, showToast]);

  useEffect(() => {
    load();
  }, [load]);

  // setVisibility 응답은 username/display_name을 채우지 않으므로(백엔드가 role만
  // 재매핑) 항상 listMembers로 다시 불러와 멤버 표시 정보를 온전히 유지한다.
  const changeVisibility = async (visibility: WorkspaceVisibility) => {
    setChangingVisibility(true);
    try {
      await api.setVisibility(activeWs, { visibility });
      showToast(
        `공개 설정이 ${visibility === 'public' ? '공개' : '비공개'}로 변경되었습니다`,
        'success',
      );
      await load();
    } catch (err) {
      showToast(err instanceof Error ? err.message : '공개 설정 변경 실패', 'error');
    } finally {
      setChangingVisibility(false);
    }
  };

  const changePublicPermission = async (permission: Permission) => {
    setChangingVisibility(true);
    try {
      await api.setVisibility(activeWs, { visibility: 'public', public_permission: permission });
      showToast('비멤버 권한이 변경되었습니다', 'success');
      await load();
    } catch (err) {
      showToast(err instanceof Error ? err.message : '비멤버 권한 변경 실패', 'error');
    } finally {
      setChangingVisibility(false);
    }
  };

  return (
    <div className="bg-card border border-border rounded-lg p-6">
      <div className="flex justify-between items-center mb-4">
        <h3 className="text-lg font-semibold text-gray-200">멤버 · 공개 설정</h3>
        {options.length > 0 && (
          <select
            className="bg-bg border border-border rounded px-3 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-primary"
            value={activeWs}
            onChange={(e) => setSelectedWs(e.target.value)}
          >
            {options.map((o) => (
              <option key={o.id} value={o.id}>
                {o.name ? `${o.name} (${o.id})` : o.id}
              </option>
            ))}
          </select>
        )}
      </div>

      {options.length === 0 ? (
        <p className="text-sm text-muted">관리 권한이 있는 워크스페이스가 없습니다.</p>
      ) : !data ? (
        <p className="text-sm text-muted">불러오는 중...</p>
      ) : (
        <>
          {/* 공개 설정 */}
          <div className="flex flex-wrap gap-3 items-center mb-4 pb-4 border-b border-border">
            <span className="text-sm text-muted">공개 범위:</span>
            <select
              className="bg-bg border border-border rounded px-3 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-primary disabled:opacity-50"
              value={data.visibility}
              disabled={changingVisibility}
              onChange={(e) => changeVisibility(e.target.value as WorkspaceVisibility)}
            >
              <option value="private">private</option>
              <option value="public">public</option>
            </select>
            {data.visibility === 'public' && (
              <>
                <span className="text-sm text-muted">비멤버 권한:</span>
                <select
                  className="bg-bg border border-border rounded px-3 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-primary disabled:opacity-50"
                  value={data.public_permission}
                  disabled={changingVisibility}
                  onChange={(e) => changePublicPermission(e.target.value as Permission)}
                >
                  {PUBLIC_PERMISSION_OPTIONS.map((p) => (
                    <option key={p} value={p}>
                      {p}
                    </option>
                  ))}
                </select>
              </>
            )}
          </div>

          {/* 멤버 목록 */}
          <div className="flex flex-col gap-2 mb-4">
            {data.members.length === 0 && (
              <p className="text-sm text-muted">멤버가 없습니다.</p>
            )}
            {data.members.map((m) => (
              <MemberRow
                key={m.user_id}
                member={m}
                workspaceId={activeWs}
                showToast={showToast}
                onChanged={load}
              />
            ))}
          </div>

          {/* 멤버 추가 */}
          <div className="border-t border-border pt-4">
            <AddMemberForm
              workspaceId={activeWs}
              existingMemberIds={new Set(data.members.map((m) => m.user_id))}
              allUsers={allUsers}
              showToast={showToast}
              onAdded={load}
            />
          </div>
        </>
      )}
    </div>
  );
}
