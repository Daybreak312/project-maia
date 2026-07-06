import { NavLink } from 'react-router-dom';
import type { WorkspaceSummary } from '../api/types';

const links = [
  { to: '/', label: 'Add' },
  { to: '/search', label: 'Search' },
  { to: '/browse', label: 'Browse' },
  { to: '/review', label: 'Review' },
  { to: '/admin', label: 'Admin' },
];

interface NavbarProps {
  workspaces: WorkspaceSummary[];
  workspace: string;
  onChangeWorkspace: (id: string) => void;
}

export function Navbar({ workspaces, workspace, onChangeWorkspace }: NavbarProps) {
  return (
    <nav className="flex justify-between items-center px-8 py-4 bg-card border-b border-border">
      <div className="flex items-center gap-4">
        <div className="flex items-center gap-2 text-xl font-bold text-primary">
          <img src="/logo.svg" alt="Maia" className="h-7 w-7" />
          Maia
        </div>
        {/* 워크스페이스 선택 — 모든 페이지 API 호출에 반영된다 */}
        <select
          className="bg-bg border border-border rounded-md px-3 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-primary"
          value={workspace}
          onChange={(e) => onChangeWorkspace(e.target.value)}
          title="Active workspace"
          aria-label="Active workspace"
        >
          {workspaces.length === 0 && <option value="">default</option>}
          {workspaces.map((ws) => (
            <option key={ws.id} value={ws.id}>
              {ws.name} ({ws.id})
            </option>
          ))}
        </select>
      </div>
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
      </div>
    </nav>
  );
}
