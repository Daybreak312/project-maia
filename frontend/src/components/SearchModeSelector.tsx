type SearchMode = 'hybrid' | 'vector' | 'keyword';

interface SearchModeSelectorProps {
  mode: SearchMode;
  onModeChange: (mode: SearchMode) => void;
}

const modes: { value: SearchMode; label: string; description: string }[] = [
  { value: 'hybrid', label: 'Hybrid', description: 'Best of both' },
  { value: 'vector', label: 'Semantic', description: 'Meaning-based' },
  { value: 'keyword', label: 'Keyword', description: 'Exact match' },
];

export function SearchModeSelector({ mode, onModeChange }: SearchModeSelectorProps) {
  return (
    <div className="flex gap-1 bg-border rounded-lg p-1">
      {modes.map((m) => (
        <button
          key={m.value}
          className={`px-3 py-1.5 rounded-md text-sm transition-colors ${
            mode === m.value
              ? 'bg-primary text-white'
              : 'text-gray-200 hover:bg-muted'
          }`}
          onClick={() => onModeChange(m.value)}
          title={m.description}
        >
          {m.label}
        </button>
      ))}
    </div>
  );
}
