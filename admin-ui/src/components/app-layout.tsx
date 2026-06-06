import { ReactNode, useState } from 'react'
import { Server, Moon, Sun, LogOut } from 'lucide-react'
import { Button } from '@/components/ui/button'

type Page = 'credentials' | 'cache-optimizer' | 'model-mapping' | 'call-log'

interface AppLayoutProps {
  currentPage: Page
  onNavigate: (page: Page) => void
  onLogout: () => void
  children: ReactNode
}

export function AppLayout({ currentPage, onNavigate, onLogout, children }: AppLayoutProps) {
  const [darkMode, setDarkMode] = useState(() => {
    if (typeof window !== 'undefined') {
      return document.documentElement.classList.contains('dark')
    }
    return false
  })

  const toggleDarkMode = () => {
    setDarkMode(!darkMode)
    document.documentElement.classList.toggle('dark')
  }

  const tabs: { key: Page; label: string }[] = [
    { key: 'credentials', label: '凭据管理' },
    { key: 'cache-optimizer', label: '模拟缓存' },
    { key: 'model-mapping', label: '模型映射' },
    { key: 'call-log', label: '调用日志' },
  ]

  return (
    <div className="min-h-screen bg-background">
      <header className="sticky top-0 z-50 w-full border-b bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/60">
        <div className="container flex h-14 items-center justify-between px-4 md:px-8">
          <div className="flex items-center gap-6">
            <div className="flex items-center gap-2">
              <Server className="h-5 w-5" />
              <span className="font-semibold">Kiro Admin</span>
            </div>
            <nav className="flex items-center gap-1">
              {tabs.map(tab => (
                <button
                  key={tab.key}
                  onClick={() => onNavigate(tab.key)}
                  className={`px-3 py-1.5 text-sm rounded-md transition-colors ${
                    currentPage === tab.key
                      ? 'bg-primary text-primary-foreground font-medium'
                      : 'text-muted-foreground hover:text-foreground hover:bg-muted'
                  }`}
                >
                  {tab.label}
                </button>
              ))}
            </nav>
          </div>
          <div className="flex items-center gap-2">
            <Button variant="ghost" size="icon" onClick={toggleDarkMode}>
              {darkMode ? <Sun className="h-5 w-5" /> : <Moon className="h-5 w-5" />}
            </Button>
            <Button variant="ghost" size="icon" onClick={onLogout}>
              <LogOut className="h-5 w-5" />
            </Button>
          </div>
        </div>
      </header>
      <main className="container mx-auto px-4 md:px-8 py-6">
        {children}
      </main>
    </div>
  )
}
