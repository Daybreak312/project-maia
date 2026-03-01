import { NavLink } from 'react-router-dom';

const links = [
  { to: '/', label: 'Add' },
  { to: '/search', label: 'Search' },
  { to: '/browse', label: 'Browse' },
  { to: '/admin', label: 'Admin' },
];

export function Navbar() {
  return (
    <nav className="flex justify-between items-center px-8 py-4 bg-card border-b border-border">
      <div className="flex items-center gap-2 text-xl font-bold text-primary">
        <img src="/logo.svg" alt="Maia" className="h-7 w-7" />
        Maia
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
