import { useState, useCallback } from 'react';
import { BrowserRouter, Routes, Route } from 'react-router-dom';
import { Navbar } from './components/Navbar';
import { Toast } from './components/Toast';
import { AddPage } from './pages/AddPage';
import { SearchPage } from './pages/SearchPage';
import { BrowsePage } from './pages/BrowsePage';
import { AdminPage } from './pages/AdminPage';

interface ToastState {
  message: string;
  type: 'success' | 'error';
}

function App() {
  const [toast, setToast] = useState<ToastState | null>(null);

  const showToast = useCallback((message: string, type: 'success' | 'error') => {
    setToast({ message, type });
  }, []);

  const hideToast = useCallback(() => {
    setToast(null);
  }, []);

  return (
    <BrowserRouter>
      <div className="min-h-screen bg-bg text-gray-200">
        <Navbar />
        <main>
          <Routes>
            <Route path="/" element={<AddPage showToast={showToast} />} />
            <Route path="/search" element={<SearchPage showToast={showToast} />} />
            <Route path="/browse" element={<BrowsePage showToast={showToast} />} />
            <Route path="/admin" element={<AdminPage showToast={showToast} />} />
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
