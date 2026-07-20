import { useState, useCallback, useEffect, useRef } from 'react';
import { BrowserRouter, Routes, Route } from 'react-router-dom';
import { Navbar } from './components/Navbar';
import type { WorkspaceOption } from './components/Navbar';
import { Toast } from './components/Toast';
import { AddPage } from './pages/AddPage';
import { SearchPage } from './pages/SearchPage';
import { BrowsePage } from './pages/BrowsePage';
import { ReviewPage } from './pages/ReviewPage';
import { AdminPage } from './pages/AdminPage';
import { AccountPage } from './pages/AccountPage';
import { LoginPage } from './pages/LoginPage';
import {
  api,
  bootstrapAuth,
  getWorkspace,
  logoutAuth,
  setOnUnauthorized,
  setWorkspace as persistWorkspace,
} from './api/client';
import type { MeResponse, WorkspaceSummary } from './api/types';

interface ToastState {
  message: string;
  type: 'success' | 'error';
}

function App() {
  const [toast, setToast] = useState<ToastState | null>(null);
  // 인증 상태: 부트스트랩 완료 전(null + loading) / 미인증(null) / 인증됨(me)
  const [me, setMe] = useState<MeResponse | null>(null);
  const [authLoading, setAuthLoading] = useState(true);
  const [workspaces, setWorkspaces] = useState<WorkspaceSummary[]>([]);
  const [workspace, setWorkspace] = useState<string>(getWorkspace());

  const showToast = useCallback((message: string, type: 'success' | 'error') => {
    setToast({ message, type });
  }, []);

  const hideToast = useCallback(() => {
    setToast(null);
  }, []);

  // 부트스트랩: 세션(쿠키) 우선 → 저장 키 폴백. 401 발생 시에도 재실행되어
  // 세션 만료·키 폐기를 로그인 화면 전환으로 수렴시킨다 (fail-closed).
  const bootstrapping = useRef(false);
  const bootstrap = useCallback(async () => {
    if (bootstrapping.current) return;
    bootstrapping.current = true;
    try {
      setMe(await bootstrapAuth());
    } finally {
      bootstrapping.current = false;
      setAuthLoading(false);
    }
  }, []);

  useEffect(() => {
    bootstrap();
  }, [bootstrap]);

  useEffect(() => {
    setOnUnauthorized(bootstrap);
    return () => setOnUnauthorized(null);
  }, [bootstrap]);

  // 워크스페이스 표시 이름 보강 — 전체 목록 조회는 글로벌 admin 전용 API라
  // admin일 때만 시도한다. 일반 계정 셀렉터는 me.workspaces의 id로 동작한다.
  const refreshWorkspaces = useCallback(async () => {
    try {
      setWorkspaces(await api.listWorkspaces());
    } catch {
      // 권한 부족(403) 등 — 이름 없이 id로만 표시해도 UI는 계속 동작한다.
    }
  }, []);

  useEffect(() => {
    if (me?.is_admin) {
      refreshWorkspaces();
    } else {
      setWorkspaces([]);
    }
  }, [me?.is_admin, refreshWorkspaces]);

  // 현재 선택이 접근 가능 목록을 벗어나면 default → 첫 항목 순으로 보정한다.
  useEffect(() => {
    if (!me) return;
    const accessible = me.workspaces.map((w) => w.id);
    const current = getWorkspace();
    if ((!current || !accessible.includes(current)) && accessible.length > 0) {
      const first = accessible.includes('default') ? 'default' : accessible[0];
      persistWorkspace(first);
      setWorkspace(first);
    }
  }, [me]);

  // 워크스페이스 전환: client 모듈 + localStorage + 렌더 상태를 함께 갱신한다.
  // page 컴포넌트는 아래 key={workspace} 리마운트로 새 워크스페이스 데이터를 재조회한다.
  const changeWorkspace = useCallback((id: string) => {
    persistWorkspace(id);
    setWorkspace(id);
  }, []);

  const handleAuthed = useCallback((authed: MeResponse) => {
    setMe(authed);
  }, []);

  const handleLogout = useCallback(async () => {
    await logoutAuth();
    setMe(null);
    showToast('로그아웃되었습니다.', 'success');
  }, [showToast]);

  // 셀렉터 옵션: 접근 가능 목록(me.workspaces)에 admin 목록의 표시 이름을 보강
  const workspaceOptions: WorkspaceOption[] = (me?.workspaces ?? []).map((w) => ({
    id: w.id,
    name: workspaces.find((s) => s.id === w.id)?.name ?? null,
  }));

  return (
    <BrowserRouter>
      {authLoading ? (
        <div className="min-h-screen bg-bg flex items-center justify-center text-muted">
          인증 확인 중...
        </div>
      ) : me ? (
        <div className="min-h-screen bg-bg text-gray-200">
          <Navbar
            me={me}
            workspaceOptions={workspaceOptions}
            workspace={workspace}
            onChangeWorkspace={changeWorkspace}
            onLogout={handleLogout}
          />
          <main key={workspace}>
            <Routes>
              <Route path="/" element={<AddPage showToast={showToast} />} />
              <Route path="/search" element={<SearchPage showToast={showToast} />} />
              <Route path="/browse" element={<BrowsePage showToast={showToast} />} />
              <Route path="/review" element={<ReviewPage showToast={showToast} />} />
              <Route
                path="/account"
                element={
                  <AccountPage me={me} showToast={showToast} onAuthChanged={bootstrap} />
                }
              />
              <Route
                path="/admin"
                element={
                  <AdminPage
                    showToast={showToast}
                    onWorkspacesChanged={refreshWorkspaces}
                  />
                }
              />
            </Routes>
          </main>
        </div>
      ) : (
        <LoginPage onAuthed={handleAuthed} />
      )}
      {toast && (
        <Toast message={toast.message} type={toast.type} onClose={hideToast} />
      )}
    </BrowserRouter>
  );
}

export default App;
