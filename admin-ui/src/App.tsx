import { useState, useEffect } from 'react'
import { storage } from '@/lib/storage'
import { LoginPage } from '@/components/login-page'
import { Dashboard } from '@/components/dashboard'
import { CacheOptimizer } from '@/components/cache-optimizer'
import { ModelMapping } from '@/components/model-mapping'
import { CallLog } from '@/components/call-log'
import { RuntimeLog } from '@/components/runtime-log'
import { Toaster } from '@/components/ui/sonner'
import { AppLayout } from '@/components/app-layout'

type Page = 'credentials' | 'cache-optimizer' | 'model-mapping' | 'call-log' | 'runtime-log'

function App() {
  const [isLoggedIn, setIsLoggedIn] = useState(false)
  const [currentPage, setCurrentPage] = useState<Page>('credentials')

  useEffect(() => {
    if (storage.getApiKey()) {
      setIsLoggedIn(true)
    }
  }, [])

  const handleLogin = () => {
    setIsLoggedIn(true)
  }

  const handleLogout = () => {
    setIsLoggedIn(false)
  }

  return (
    <>
      {isLoggedIn ? (
        <AppLayout currentPage={currentPage} onNavigate={setCurrentPage} onLogout={handleLogout}>
          {currentPage === 'credentials' && <Dashboard />}
          {currentPage === 'cache-optimizer' && <CacheOptimizer />}
          {currentPage === 'model-mapping' && <ModelMapping />}
          {currentPage === 'call-log' && <CallLog />}
          {currentPage === 'runtime-log' && <RuntimeLog />}
        </AppLayout>
      ) : (
        <LoginPage onLogin={handleLogin} />
      )}
      <Toaster position="top-right" />
    </>
  )
}

export default App
