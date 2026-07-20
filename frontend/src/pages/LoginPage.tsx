import { useState } from 'react';
import type { FormEvent } from 'react';
import { ApiError, getAuthKey, loginWithKey, loginWithPassword } from '../api/client';
import type { MeResponse } from '../api/types';

interface LoginPageProps {
  /** 인증 성공 시 산출된 me를 앱에 전달한다 (게이트 해제). */
  onAuthed: (me: MeResponse) => void;
}

/**
 * 미인증(401) 상태의 진입점. 라우트 교체가 아니라 게이트로 렌더되므로
 * 로그인 성공 시 사용자가 보던 URL이 그대로 유지된다.
 *
 * 기본 경로는 ID/PW 세션 로그인이고, 마스터키·발급 키로 접속하는
 * 레거시 경로를 보조로 제공한다 (계정이 아직 없는 초기 구축 단계 포함).
 */
export function LoginPage({ onAuthed }: LoginPageProps) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // API 키 보조 경로 (접혀 있음)
  const [showKeyPath, setShowKeyPath] = useState(false);
  const [keyInput, setKeyInput] = useState('');
  const storedKey = getAuthKey();

  const submitLogin = async (e: FormEvent) => {
    e.preventDefault();
    if (!username.trim() || !password) return;
    setBusy(true);
    setError(null);
    try {
      onAuthed(await loginWithPassword(username.trim(), password));
    } catch (err) {
      // 서버가 계정 존재 여부를 구분하지 않는 단일 401을 주므로,
      // 클라이언트도 단일 문구로만 안내한다 (열거 방지 유지).
      if (err instanceof ApiError && err.status === 401) {
        setError('아이디 또는 비밀번호가 올바르지 않습니다.');
      } else {
        setError('로그인에 실패했습니다 — 서버 연결을 확인해 주세요.');
      }
    } finally {
      setBusy(false);
    }
  };

  const submitKey = async (key: string) => {
    if (!key) return;
    setBusy(true);
    setError(null);
    try {
      onAuthed(await loginWithKey(key));
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) {
        setError('유효하지 않은 API 키입니다.');
      } else {
        setError('키 확인에 실패했습니다 — 서버 연결을 확인해 주세요.');
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="min-h-screen bg-bg text-gray-200 flex items-center justify-center p-6">
      <div className="w-full max-w-sm">
        <div className="flex items-center justify-center gap-2 text-2xl font-bold text-primary mb-8">
          <img src="/logo.svg" alt="Maia" className="h-8 w-8" />
          Maia
        </div>

        <form
          className="bg-card border border-border rounded-lg p-6 flex flex-col gap-3"
          onSubmit={submitLogin}
        >
          <h2 className="text-lg font-semibold text-gray-200 mb-1">로그인</h2>
          <input
            className="bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
            placeholder="아이디 (username)"
            autoComplete="username"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            autoFocus
          />
          <input
            type="password"
            className="bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
            placeholder="비밀번호"
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
          {error && <p className="text-sm text-error">{error}</p>}
          <button
            type="submit"
            className="px-4 py-2 bg-primary text-white text-sm rounded hover:bg-primary-hover transition-colors disabled:opacity-50"
            disabled={busy || !username.trim() || !password}
          >
            {busy ? '확인 중...' : '로그인'}
          </button>
        </form>

        {/* 보조 경로: 마스터키/발급 키 직접 접속 (계정 없는 초기 구축·머신 관리용) */}
        <div className="mt-4 text-center text-sm text-muted">
          {storedKey && (
            <button
              className="block w-full px-4 py-2 mb-2 bg-border text-gray-200 rounded hover:bg-muted transition-colors disabled:opacity-50"
              onClick={() => submitKey(storedKey)}
              disabled={busy}
            >
              저장된 API 키로 계속
            </button>
          )}
          {showKeyPath ? (
            <div className="bg-card border border-border rounded-lg p-4 flex flex-col gap-2 text-left">
              <p className="text-xs text-muted">
                마스터키 또는 발급된 API 키로 접속합니다. 키는 이 브라우저에만 저장됩니다.
              </p>
              <div className="flex gap-2">
                <input
                  type="password"
                  className="flex-1 bg-bg border border-border rounded px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-primary"
                  placeholder="API 키 붙여넣기..."
                  value={keyInput}
                  onChange={(e) => setKeyInput(e.target.value)}
                />
                <button
                  className="px-4 py-2 bg-primary text-white text-sm rounded hover:bg-primary-hover transition-colors disabled:opacity-50"
                  onClick={() => submitKey(keyInput.trim())}
                  disabled={busy || !keyInput.trim()}
                >
                  접속
                </button>
              </div>
            </div>
          ) : (
            <button
              className="text-xs text-muted hover:text-gray-200 transition-colors"
              onClick={() => setShowKeyPath(true)}
            >
              API 키로 접속…
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
