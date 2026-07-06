import { useState, useCallback, useEffect } from 'react';
import { BrowserRouter, Routes, Route } from 'react-router-dom';
import { Navbar } from './components/Navbar';
import { Toast } from './components/Toast';
import { AddPage } from './pages/AddPage';
import { SearchPage } from './pages/SearchPage';
import { BrowsePage } from './pages/BrowsePage';
import { AdminPage } from './pages/AdminPage';
import { api, getWorkspace, setWorkspace as persistWorkspace } from './api/client';
import type { WorkspaceSummary } from './api/types';

interface ToastState {
  message: string;
  type: 'success' | 'error';
}

function App() {
  const [toast, setToast] = useState<ToastState | null>(null);
  const [workspaces, setWorkspaces] = useState<WorkspaceSummary[]>([]);
  const [workspace, setWorkspace] = useState<string>(getWorkspace());

  const showToast = useCallback((message: string, type: 'success' | 'error') => {
    setToast({ message, type });
  }, []);

  const hideToast = useCallback(() => {
    setToast(null);
  }, []);

  const refreshWorkspaces = useCallback(async () => {
    try {
      const list = await api.listWorkspaces();
      setWorkspaces(list);
      // 현재 선택이 비어있거나 목록에 없으면 첫 워크스페이스로 맞춘다.
      const current = getWorkspace();
      if ((!current || !list.some((w) => w.id === current)) && list.length > 0) {
        const first = list.find((w) => w.id === 'default')?.id ?? list[0].id;
        persistWorkspace(first);
        setWorkspace(first);
      }
    } catch {
      // 인증/권한 문제로 목록을 못 가져와도 UI는 계속 동작한다 (dev 모드 등).
    }
  }, []);

  useEffect(() => {
    refreshWorkspaces();
  }, [refreshWorkspaces]);

  // 워크스페이스 전환: client 모듈 + localStorage + 렌더 상태를 함께 갱신한다.
  // page 컴포넌트는 아래 key={workspace} 리마운트로 새 워크스페이스 데이터를 재조회한다.
  const changeWorkspace = useCallback((id: string) => {
    persistWorkspace(id);
    setWorkspace(id);
  }, []);

  return (
    <BrowserRouter>
      <div className="min-h-screen bg-bg text-gray-200">
        <Navbar
          workspaces={workspaces}
          workspace={workspace}
          onChangeWorkspace={changeWorkspace}
        />
        <main key={workspace}>
          <Routes>
            <Route path="/" element={<AddPage showToast={showToast} />} />
            <Route path="/search" element={<SearchPage showToast={showToast} />} />
            <Route path="/browse" element={<BrowsePage showToast={showToast} />} />
            <Route
              path="/admin"
              element={
                <AdminPage showToast={showToast} onWorkspacesChanged={refreshWorkspaces} />
              }
            />
          </Routes>
        </main>
        {toast && (
          <Toast message={toast.message} type={toast.type} onClose={hideToast} />
        )}
      </div>
    </BrowserRouter>
  );
}

export default App;
