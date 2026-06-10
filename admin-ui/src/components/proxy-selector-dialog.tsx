import { useEffect, useState } from 'react'
import { ArrowDown, ArrowUp, Check, Network, X } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import type { ProxyProfile } from '@/types/api'

interface ProxySelectorDialogProps {
  open: boolean
  title: string
  proxies: ProxyProfile[]
  initialProxyIds: number[]
  saving?: boolean
  onOpenChange: (open: boolean) => void
  onSave: (proxyIds: number[]) => void
}

function proxyLabel(proxy: ProxyProfile) {
  return `${proxy.name || `代理 #${proxy.id}`} · ${proxy.protocol.toUpperCase()} ${proxy.host}:${proxy.port}`
}

export function ProxySelectorDialog({
  open,
  title,
  proxies,
  initialProxyIds,
  saving = false,
  onOpenChange,
  onSave,
}: ProxySelectorDialogProps) {
  const [selectedIds, setSelectedIds] = useState<number[]>([])

  useEffect(() => {
    if (!open) return
    const valid = new Set(proxies.map(proxy => proxy.id))
    setSelectedIds(initialProxyIds.filter(id => valid.has(id)))
  }, [open, initialProxyIds, proxies])

  const toggleProxy = (id: number) => {
    setSelectedIds(prev => {
      if (prev.includes(id)) return prev.filter(item => item !== id)
      return [...prev, id]
    })
  }

  const move = (id: number, direction: -1 | 1) => {
    setSelectedIds(prev => {
      const index = prev.indexOf(id)
      const nextIndex = index + direction
      if (index < 0 || nextIndex < 0 || nextIndex >= prev.length) return prev
      const next = [...prev]
      const [item] = next.splice(index, 1)
      next.splice(nextIndex, 0, item)
      return next
    })
  }

  const selectedProxies = selectedIds
    .map(id => proxies.find(proxy => proxy.id === id))
    .filter((proxy): proxy is ProxyProfile => Boolean(proxy))

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Network className="h-5 w-5" />
            {title}
          </DialogTitle>
        </DialogHeader>

        <div className="grid gap-4 md:grid-cols-[1fr_1fr]">
          <div className="space-y-2">
            <div className="text-sm font-medium">可用代理</div>
            <div className="max-h-72 overflow-y-auto rounded-md border divide-y">
              {proxies.length === 0 ? (
                <div className="p-4 text-sm text-muted-foreground">
                  暂无代理，请先到代理管理中新增代理
                </div>
              ) : (
                proxies.map(proxy => (
                  <label
                    key={proxy.id}
                    className="flex cursor-pointer items-start gap-3 p-3 hover:bg-muted/60"
                  >
                    <Checkbox
                      checked={selectedIds.includes(proxy.id)}
                      onCheckedChange={() => toggleProxy(proxy.id)}
                    />
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-sm font-medium">{proxy.name || `代理 #${proxy.id}`}</div>
                      <div className="truncate font-mono text-xs text-muted-foreground">
                        {proxy.protocol.toUpperCase()} {proxy.host}:{proxy.port}
                      </div>
                    </div>
                  </label>
                ))
              )}
            </div>
          </div>

          <div className="space-y-2">
            <div className="text-sm font-medium">使用优先级</div>
            <div className="max-h-72 overflow-y-auto rounded-md border divide-y">
              {selectedProxies.length === 0 ? (
                <div className="p-4 text-sm text-muted-foreground">
                  未选择代理，将回退到凭据旧代理或全局代理
                </div>
              ) : (
                selectedProxies.map((proxy, index) => (
                  <div key={proxy.id} className="flex items-center gap-2 p-3">
                    <div className="flex h-6 w-6 items-center justify-center rounded bg-muted text-xs font-medium">
                      {index + 1}
                    </div>
                    <div className="min-w-0 flex-1 text-sm">
                      <div className="truncate font-medium">{proxyLabel(proxy)}</div>
                    </div>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      className="h-7 w-7"
                      onClick={() => move(proxy.id, -1)}
                      disabled={index === 0}
                    >
                      <ArrowUp className="h-4 w-4" />
                    </Button>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      className="h-7 w-7"
                      onClick={() => move(proxy.id, 1)}
                      disabled={index === selectedProxies.length - 1}
                    >
                      <ArrowDown className="h-4 w-4" />
                    </Button>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      className="h-7 w-7"
                      onClick={() => toggleProxy(proxy.id)}
                    >
                      <X className="h-4 w-4" />
                    </Button>
                  </div>
                ))
              )}
            </div>
          </div>
        </div>

        <DialogFooter>
          <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={saving}>
            取消
          </Button>
          <Button type="button" variant="outline" onClick={() => setSelectedIds([])} disabled={saving}>
            清空代理
          </Button>
          <Button type="button" onClick={() => onSave(selectedIds)} disabled={saving}>
            <Check className="h-4 w-4" />
            {saving ? '保存中...' : '保存'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
