import { NavLink } from 'react-router-dom';
import type { MeResponse } from '../api/types';

const links = [
  { to: '/', label: 'Add' },
  { to: '/search', label: 'Search' },
  { to: '/browse', label: 'Browse' },
  { to: '/review', label: 'Review' },
];

/** 셀렉터 한 항목 — me.workspaces 기반 + (admin이면) 표시 이름 보강. */
export interface WorkspaceOption {
  id: string;
  /** WorkspaceConfig에서 가져온 표시 이름 (admin 목록 조회 가능 시에만). */
  name: string | null;
}

interface NavbarProps {
  me: MeResponse;
  workspaceOptions: WorkspaceOption[];
  workspace: string;
  onChangeWorkspace: (id: string) => void;
  onLogout: () => void;
}

export function Navbar({
  me,
  workspaceOptions,
  workspace,
  onChangeWorkspace,
  onLogout,
}: NavbarProps) {
  // Admin 링크는 관리할 수 있는 것이 있는 사용자에게만 (서버가 진실 — UI는 편의)
  const canAdmin =
    me.is_admin || me.workspaces.some((w) => w.permission === 'admin');
  // 계정 표시명: 세션/소유 키는 계정 이름, 마스터키·dev는 인증 소스 라벨
  const identityLabel =
    me.user?.display_name ??
    (me.auth_source === 'master' ? '마스터키' : me.auth_source);

  return (
    <nav className="flex justify-between items-center px-8 py-4 bg-card border-b border-border">
      <div className="flex items-center gap-4">
        <div className="flex items-center gap-2 text-xl font-bold text-primary">
          <img src="/logo.svg" alt="Maia" className="h-7 w-7" />
          Maia
        </div>
        {/* 워크스페이스 선택 — 접근 가능 목록(me.workspaces) 기반, 모든 페이지 API 호출에 반영 */}
        <select
          className="bg-bg border border-border rounded-md px-3 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-primary"
          value={workspace}
          onChange={(e) => onChangeWorkspace(e.target.value)}
          title="Active workspace"
          aria-label="Active workspace"
        >
          {workspaceOptions.length === 0 && (
            <option value="">(접근 가능한 워크스페이스 없음)</option>
          )}
          {workspaceOptions.map((ws) => (
            <option key={ws.id} value={ws.id}>
              {ws.name ? `${ws.name} (${ws.id})` : ws.id}
            </option>
          ))}
        </select>
      </div>
      <div className="flex items-center gap-6">
        <div className="flex gap-6">
          {links.map((link) => (
            <NavLink
              key={link.to}
              to={link.to}
              className={({ isActive }) =>
                `px-4 py-2 rounded-md transition-colors ${
                  isActive
                    ? 'text-primary bg-primary/10'
                    : 'text-muted hover:text-gray-200 hover:bg-border'
                }`
              }
            >
              {link.label}
            </NavLink>
          ))}
          {canAdmin && (
            <NavLink
              to="/admin"
              className={({ isActive }) =>
                `px-4 py-2 rounded-md transition-colors ${
                  isActive
                    ? 'text-primary bg-primary/10'
                    : 'text-muted hover:text-gray-200 hover:bg-border'
                }`
              }
            >
              Admin
            </NavLink>
          )}
        </div>
        {/* 계정 영역: 내 계정 페이지 + 로그아웃 */}
        <div className="flex items-center gap-2 border-l border-border pl-4">
          <NavLink
            to="/account"
            className={({ isActive }) =>
              `flex items-center gap-1.5 px-3 py-2 border rounded-md text-sm transition-colors ${
                isActive
                  ? 'text-primary bg-primary/10 border-primary/40'
                  : 'text-muted border-border hover:text-gray-200 hover:bg-border'
              }`
            }
            title="내 계정 — 인증 정보"
          >
            <svg
              className="h-4 w-4"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.8"
              aria-hidden="true"
            >
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                d="M15.75 6a3.75 3.75 0 1 1-7.5 0 3.75 3.75 0 0 1 7.5 0ZM4.5 20.25a7.5 7.5 0 0 1 15 0"
              />
            </svg>
            {identityLabel}
          </NavLink>
          <button
            className="px-3 py-2 text-sm text-muted rounded-md hover:text-gray-200 hover:bg-border transition-colors"
            onClick={onLogout}
            title="로그아웃"
          >
            로그아웃
          </button>
        </div>
      </div>
    </nav>
  );
}
