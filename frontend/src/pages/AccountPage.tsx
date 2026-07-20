import { useState, useEffect, useCallback } from 'react';
import { api } from '../api/client';
import type {
  ApiKeyInfo,
  CreateKeyResponse,
  MeResponse,
  Permission,
} from '../api/types';

interface AccountPageProps {
  me: MeResponse;
  showToast: (message: string, type: 'success' | 'error') => void;
  /** 인증 상태 재산출 트리거 (비밀번호 변경 → 전 세션 폐기 → 재로그인 유도). */
  onAuthChanged: () => void;
}

/** 백엔드 검증 규칙과 동일 (auth/users.rs MIN_PASSWORD_LEN) — 사전 안내용. */
const MIN_PASSWORD_LEN = 8;

const AUTH_SOURCE_LABEL: Record<MeResponse['auth_source'], string> = {
  session: '로그인 세션',
  api_key: 'API 키',
  master: '마스터키',
  dev: '개발 모드 (인증 비활성)',
};

// ─────────────────────────────────────────────────────────────────────
// 비밀번호 변경 — 본인 세션 전용. 성공 시 서버가 이 계정의 모든 세션을
// 폐기하므로 재로그인 흐름으로 이어진다.
// ─────────────────────────────────────────────────────────────────────
function PasswordSection({
  userId,
  showToast,
  onAuthChanged,
}: {
  userId: string;
  showToast: AccountPageProps['showToast'];
  onAuthChanged: () => void;
}) {
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');
  const [busy, setBusy] = useState(false);

  const mismatch = confirm.length > 0 && password !== confirm;
  const tooShort = password.length > 0 && password.length < MIN_PASSWORD_LEN;
  const canSubmit =
    !busy && password.length >= MIN_PASSWORD_LEN && password === confirm;

  const submit = async () => {
    if (!canSubmit) return;
    setBusy(true);
    try {
      await api.changePassword(userId, password);
      // 서버가 전 세션을 폐기했다 — 재부트스트랩하면 로그인 화면으로 수렴한다.
      showToast('비밀번호가 변경되었습니다. 새 비밀번호로 다시 로그인해 주세요.', 'success');
      onAuthChanged();
    } catch (err) {
      showToast(err instanceof Error ? err.message : '비밀번호 변경 실패', 'error');
      setBusy(false);
    }
  };

  return (
    <div className="bg-card border border-border rounded-lg p-6">
      <h3 className="text-lg font-semibold text-gray-200 mb-2">비밀번호 변경</h3>
      <p className="text-sm text-muted mb-4">
        변경하면 이 계정의 모든 로그인 세션이 종료되어 다시 로그인해야 합니다.
      </p>
      <div className="flex flex-col gap-2 max-w-sm">
        <input
          type="password"
          className="bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
          placeholder={`새 비밀번호 (${MIN_PASSWORD_LEN}자 이상)`}
          autoComplete="new-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        <input
          type="password"
          className="bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
          placeholder="새 비밀번호 확인"
          autoComplete="new-password"
          value={confirm}
          onChange={(e) => setConfirm(e.target.value)}
        />
        {tooShort && (
          <p className="text-xs text-error">비밀번호는 {MIN_PASSWORD_LEN}자 이상이어야 합니다.</p>
        )}
        {mismatch && <p className="text-xs text-error">비밀번호가 일치하지 않습니다.</p>}
        <button
          className="px-4 py-2 bg-primary text-white text-sm rounded hover:bg-primary-hover transition-colors disabled:opacity-50 self-start"
          onClick={submit}
          disabled={!canSubmit}
        >
          {busy ? '변경 중...' : '비밀번호 변경'}
        </button>
      </div>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────
// 내 API 키 — /api/me/keys 셀프서비스 (로그인 세션 전용).
// 발급 직후 평문 1회 노출, 이후 목록엔 key_id/label만.
// ─────────────────────────────────────────────────────────────────────
function MyKeysSection({
  me,
  showToast,
}: {
  me: MeResponse;
  showToast: AccountPageProps['showToast'];
}) {
  const [keys, setKeys] = useState<ApiKeyInfo[]>([]);
  const [label, setLabel] = useState('');
  const [permission, setPermission] = useState<Permission>('read_write');
  const [selectedWs, setSelectedWs] = useState<string[]>([]);
  const [creating, setCreating] = useState(false);
  const [issuedKey, setIssuedKey] = useState<CreateKeyResponse | null>(null);

  const load = useCallback(async () => {
    try {
      setKeys(await api.listMyKeys());
    } catch (err) {
      showToast(err instanceof Error ? err.message : '키 목록 조회 실패', 'error');
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
    // 스코프 없는 키는 서버가 400으로 거부한다 (fail-closed) — 버튼도 막는다.
    if (!label.trim() || selectedWs.length === 0) return;
    setCreating(true);
    try {
      const res = await api.createMyKey({
        label: label.trim(),
        workspaces: selectedWs,
        permissions: permission,
      });
      setIssuedKey(res);
      showToast(`'${label.trim()}' 키가 발급되었습니다`, 'success');
      setLabel('');
      setSelectedWs([]);
      await load();
    } catch (err) {
      // 접근권 초과 스코프/권한은 서버가 400 + 사유 메시지로 알려준다.
      showToast(err instanceof Error ? err.message : '키 발급 실패', 'error');
    } finally {
      setCreating(false);
    }
  };

  const revoke = async (keyId: string, keyLabel: string) => {
    if (!confirm(`'${keyLabel}' (${keyId}) 키를 폐기할까요?`)) return;
    try {
      await api.revokeMyKey(keyId);
      showToast('키가 폐기되었습니다', 'success');
      await load();
    } catch (err) {
      showToast(err instanceof Error ? err.message : '키 폐기 실패', 'error');
    }
  };

  return (
    <div className="bg-card border border-border rounded-lg p-6">
      <h3 className="text-lg font-semibold text-gray-200 mb-2">내 API 키</h3>
      <p className="text-sm text-muted mb-4">
        MCP·머신 접근용 개인 키입니다. 내 접근 권한 범위 안에서만 발급되며, 계정
        권한이 줄면 키의 유효 권한도 함께 줄어듭니다.
      </p>

      {/* 방금 발급된 평문 키 — 1회성 표시 */}
      {issuedKey && (
        <div className="bg-primary/10 border border-primary rounded p-4 mb-4">
          <p className="text-sm text-gray-200 mb-2">
            지금 복사해 두세요 — 이 키는 다시 표시되지 않습니다:
          </p>
          <code className="block bg-bg border border-border rounded px-3 py-2 text-sm text-primary break-all">
            {issuedKey.api_key}
          </code>
          <button
            className="mt-2 px-3 py-1 bg-border text-gray-200 text-xs rounded hover:bg-muted transition-colors"
            onClick={() => setIssuedKey(null)}
          >
            닫기
          </button>
        </div>
      )}

      <div className="flex flex-col gap-2 mb-4">
        {keys.length === 0 && (
          <p className="text-sm text-muted">발급된 키가 없습니다.</p>
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
                마지막 사용: {k.last_used_at ? new Date(k.last_used_at).toLocaleString() : '없음'}
              </div>
            </div>
            <button
              className="px-3 py-1 bg-error text-white text-xs rounded hover:bg-red-700 transition-colors"
              onClick={() => revoke(k.key_id, k.label)}
            >
              폐기
            </button>
          </div>
        ))}
      </div>

      {/* 발급 폼 — 스코프 후보는 내 접근 가능 워크스페이스로 한정 */}
      <div className="border-t border-border pt-4 flex flex-col gap-3">
        <div className="flex flex-wrap gap-2 items-center">
          <input
            className="w-48 bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
            placeholder="라벨 (예: iPad)"
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
            {creating ? '발급 중...' : '키 발급'}
          </button>
        </div>
        {me.workspaces.length > 0 ? (
          <div className="flex flex-wrap gap-3 text-sm text-muted">
            <span>스코프:</span>
            {me.workspaces.map((ws) => (
              <label key={ws.id} className="flex items-center gap-1 cursor-pointer">
                <input
                  type="checkbox"
                  checked={selectedWs.includes(ws.id)}
                  onChange={() => toggleWs(ws.id)}
                />
                {ws.id}
                <span className="text-xs">({ws.permission})</span>
              </label>
            ))}
            <span className="text-xs">
              (1개 이상 필수 — 내 권한을 초과하는 스코프/권한은 거부됩니다)
            </span>
          </div>
        ) : (
          <p className="text-xs text-muted">
            접근 가능한 워크스페이스가 없어 키를 발급할 수 없습니다.
          </p>
        )}
      </div>
    </div>
  );
}

// ─────────────────────────────────────────────────────────────────────
// 내 계정 페이지 — me 정보 + 비밀번호 변경 + 내 API 키.
// 계정 기능(비밀번호·키)은 로그인 세션 인증에서만 동작한다
// (키 인증으로는 서버가 403 — 유출 키의 자기 증식 차단).
// ─────────────────────────────────────────────────────────────────────
export function AccountPage({ me, showToast, onAuthChanged }: AccountPageProps) {
  const isSession = me.auth_source === 'session';

  return (
    <div className="max-w-4xl mx-auto p-6">
      <h2 className="text-xl font-semibold text-gray-200 mb-6">내 계정</h2>

      <div className="flex flex-col gap-4">
        {/* 계정 정보 */}
        <div className="bg-card border border-border rounded-lg p-6">
          <h3 className="text-lg font-semibold text-gray-200 mb-4">계정 정보</h3>
          {me.user ? (
            <div className="text-sm text-gray-200 flex flex-col gap-1">
              <div>
                <span className="text-muted">이름:</span> {me.user.display_name}{' '}
                {me.user.is_admin && (
                  <span className="text-xs bg-primary/20 text-primary px-2 py-0.5 rounded ml-1">
                    admin
                  </span>
                )}
              </div>
              <div>
                <span className="text-muted">아이디:</span>{' '}
                <span className="font-mono">{me.user.username}</span>{' '}
                <span className="text-xs text-muted font-mono">({me.user.user_id})</span>
              </div>
              <div>
                <span className="text-muted">가입일:</span>{' '}
                {new Date(me.user.created_at).toLocaleString()}
              </div>
              <div>
                <span className="text-muted">인증 방식:</span>{' '}
                {AUTH_SOURCE_LABEL[me.auth_source]}
              </div>
            </div>
          ) : (
            <p className="text-sm text-muted">
              {AUTH_SOURCE_LABEL[me.auth_source]} 인증 중 — 연결된 계정이 없습니다.
              계정 기능(비밀번호·개인 키)은 ID/PW 로그인 세션에서 제공됩니다.
            </p>
          )}

          {/* 접근 가능 워크스페이스 요약 */}
          {me.workspaces.length > 0 && (
            <div className="mt-4 pt-4 border-t border-border">
              <p className="text-sm text-muted mb-2">접근 가능한 워크스페이스</p>
              <div className="flex flex-wrap gap-2">
                {me.workspaces.map((ws) => (
                  <span
                    key={ws.id}
                    className="text-xs bg-bg border border-border rounded px-2 py-1 text-gray-200"
                  >
                    <span className="font-mono">{ws.id}</span>{' '}
                    <span className="text-muted">· {ws.permission}</span>
                  </span>
                ))}
              </div>
            </div>
          )}
        </div>

        {isSession && me.user ? (
          <>
            <PasswordSection
              userId={me.user.user_id}
              showToast={showToast}
              onAuthChanged={onAuthChanged}
            />
            <MyKeysSection me={me} showToast={showToast} />
          </>
        ) : (
          me.user && (
            <div className="bg-card border border-border rounded-lg p-6">
              <p className="text-sm text-muted">
                비밀번호 변경과 개인 API 키 관리는 로그인 세션에서만 가능합니다.
                로그아웃 후 ID/PW로 로그인해 주세요.
              </p>
            </div>
          )
        )}
      </div>
    </div>
  );
}
