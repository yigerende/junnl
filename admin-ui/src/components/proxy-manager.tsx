import { useEffect, useState } from 'react'
import { CheckCircle2, Loader2, Network, Pencil, Plus, RefreshCw, Trash2 } from 'lucide-react'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  useCreateProxy,
  useDeleteProxy,
  useProxies,
  useTestProxy,
  useUpdateProxy,
} from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { ProxyProfile } from '@/types/api'

type ProxyForm = Omit<ProxyProfile, 'id'>

const emptyForm: ProxyForm = {
  name: '',
  protocol: 'http',
  host: '',
  port: 7890,
  username: '',
  password: '',
}

function displayProxyUrl(proxy: ProxyProfile) {
  const auth = proxy.username || proxy.password ? `${proxy.username || ''}${proxy.password ? ':******' : ''}@` : ''
  return `${proxy.protocol}://${auth}${proxy.host}:${proxy.port}`
}

export function ProxyManager() {
  const { data: proxies = [], isLoading, refetch } = useProxies()
  const createProxy = useCreateProxy()
  const updateProxy = useUpdateProxy()
  const deleteProxy = useDeleteProxy()
  const testProxy = useTestProxy()
  const [dialogOpen, setDialogOpen] = useState(false)
  const [editingProxy, setEditingProxy] = useState<ProxyProfile | null>(null)
  const [form, setForm] = useState<ProxyForm>(emptyForm)
  const [testingIds, setTestingIds] = useState<Set<number>>(new Set())

  useEffect(() => {
    if (!dialogOpen) return
    if (editingProxy) {
      setForm({
        name: editingProxy.name || '',
        protocol: editingProxy.protocol,
        host: editingProxy.host,
        port: editingProxy.port,
        username: editingProxy.username || '',
        password: editingProxy.password || '',
      })
    } else {
      setForm(emptyForm)
    }
  }, [dialogOpen, editingProxy])

  const openCreate = () => {
    setEditingProxy(null)
    setDialogOpen(true)
  }

  const openEdit = (proxy: ProxyProfile) => {
    setEditingProxy(proxy)
    setDialogOpen(true)
  }

  const handleSubmit = () => {
    const normalized: ProxyForm = {
      name: form.name.trim(),
      protocol: form.protocol,
      host: form.host.trim(),
      port: Number(form.port),
      username: form.username?.trim() || undefined,
      password: form.password?.trim() || undefined,
    }
    if (!normalized.host) {
      toast.error('请输入代理主机')
      return
    }
    if (!Number.isInteger(normalized.port) || normalized.port < 1 || normalized.port > 65535) {
      toast.error('端口必须在 1-65535 之间')
      return
    }

    const mutation = editingProxy
      ? updateProxy.mutateAsync({ id: editingProxy.id, proxy: normalized })
      : createProxy.mutateAsync(normalized)

    mutation
      .then(() => {
        toast.success(editingProxy ? '代理已更新' : '代理已新增')
        setDialogOpen(false)
      })
      .catch((error) => toast.error('保存失败: ' + extractErrorMessage(error)))
  }

  const handleDelete = (proxy: ProxyProfile) => {
    if (!confirm(`确定删除代理「${proxy.name || proxy.host}」吗？`)) return
    deleteProxy.mutate(proxy.id, {
      onSuccess: (res) => toast.success(res.message),
      onError: (error) => toast.error('删除失败: ' + extractErrorMessage(error)),
    })
  }

  const handleTest = async (proxy: ProxyProfile) => {
    setTestingIds(prev => new Set(prev).add(proxy.id))
    try {
      const result = await testProxy.mutateAsync(proxy.id)
      if (result.success) {
        toast.success(
          `代理可用${result.latencyMs ? `，${result.latencyMs}ms` : ''}${result.ipAddress ? `，出口 ${result.ipAddress}` : ''}`
        )
      } else {
        toast.error(result.message)
      }
    } catch (error) {
      toast.error('测试失败: ' + extractErrorMessage(error))
    } finally {
      setTestingIds(prev => {
        const next = new Set(prev)
        next.delete(proxy.id)
        return next
      })
    }
  }

  return (
    <div className="space-y-5">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-xl font-semibold">代理管理</h2>
          <p className="text-sm text-muted-foreground mt-1">集中维护代理池，凭据可选择多个代理并按优先级使用。</p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={() => refetch()} disabled={isLoading}>
            <RefreshCw className={`h-4 w-4 ${isLoading ? 'animate-spin' : ''}`} />
            刷新
          </Button>
          <Button size="sm" onClick={openCreate}>
            <Plus className="h-4 w-4" />
            新增代理
          </Button>
        </div>
      </div>

      {isLoading ? (
        <Card>
          <CardContent className="py-10 text-center text-muted-foreground">
            <Loader2 className="mx-auto mb-2 h-5 w-5 animate-spin" />
            加载代理中...
          </CardContent>
        </Card>
      ) : proxies.length === 0 ? (
        <Card>
          <CardContent className="py-10 text-center text-muted-foreground">
            暂无代理
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {proxies.map(proxy => (
            <Card key={proxy.id}>
              <CardHeader className="pb-3">
                <CardTitle className="flex items-center justify-between text-base">
                  <span className="min-w-0 truncate">{proxy.name || `代理 #${proxy.id}`}</span>
                  <Badge variant="secondary">{proxy.protocol.toUpperCase()}</Badge>
                </CardTitle>
              </CardHeader>
              <CardContent className="space-y-4">
                <div className="rounded-md border bg-muted/40 p-3 font-mono text-xs break-all">
                  {displayProxyUrl(proxy)}
                </div>
                <div className="grid grid-cols-2 gap-2 text-sm">
                  <div>
                    <span className="text-muted-foreground">主机：</span>
                    <span className="font-medium">{proxy.host}</span>
                  </div>
                  <div>
                    <span className="text-muted-foreground">端口：</span>
                    <span className="font-medium">{proxy.port}</span>
                  </div>
                  <div className="col-span-2">
                    <span className="text-muted-foreground">认证：</span>
                    <span className="font-medium">{proxy.username || proxy.password ? '已配置' : '无'}</span>
                  </div>
                </div>
                <div className="flex flex-wrap gap-2 border-t pt-3">
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => handleTest(proxy)}
                    disabled={testingIds.has(proxy.id)}
                  >
                    {testingIds.has(proxy.id) ? (
                      <Loader2 className="h-4 w-4 animate-spin" />
                    ) : (
                      <CheckCircle2 className="h-4 w-4" />
                    )}
                    测试连接
                  </Button>
                  <Button size="sm" variant="outline" onClick={() => openEdit(proxy)}>
                    <Pencil className="h-4 w-4" />
                    编辑
                  </Button>
                  <Button
                    size="sm"
                    variant="destructive"
                    onClick={() => handleDelete(proxy)}
                    disabled={deleteProxy.isPending}
                  >
                    <Trash2 className="h-4 w-4" />
                    删除
                  </Button>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}

      <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Network className="h-5 w-5" />
              {editingProxy ? '编辑代理' : '新增代理'}
            </DialogTitle>
          </DialogHeader>

          <div className="space-y-4">
            <div className="space-y-2">
              <label className="text-sm font-medium">名称</label>
              <Input
                value={form.name}
                onChange={(e) => setForm(prev => ({ ...prev, name: e.target.value }))}
                placeholder="留空自动使用 host:port"
              />
            </div>
            <div className="space-y-2">
              <label className="text-sm font-medium">协议</label>
              <select
                value={form.protocol}
                onChange={(e) => setForm(prev => ({ ...prev, protocol: e.target.value as ProxyForm['protocol'] }))}
                className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
              >
                <option value="http">HTTP</option>
                <option value="https">HTTPS</option>
                <option value="socks5">SOCKS5</option>
              </select>
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div className="space-y-2">
                <label className="text-sm font-medium">主机</label>
                <Input
                  value={form.host}
                  onChange={(e) => setForm(prev => ({ ...prev, host: e.target.value }))}
                  placeholder="127.0.0.1"
                />
              </div>
              <div className="space-y-2">
                <label className="text-sm font-medium">端口</label>
                <Input
                  type="number"
                  min="1"
                  max="65535"
                  value={String(form.port)}
                  onChange={(e) => setForm(prev => ({ ...prev, port: parseInt(e.target.value, 10) || 0 }))}
                  placeholder="7890"
                />
              </div>
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div className="space-y-2">
                <label className="text-sm font-medium">用户名</label>
                <Input
                  value={form.username || ''}
                  onChange={(e) => setForm(prev => ({ ...prev, username: e.target.value }))}
                  placeholder="可选"
                />
              </div>
              <div className="space-y-2">
                <label className="text-sm font-medium">密码</label>
                <Input
                  type="password"
                  value={form.password || ''}
                  onChange={(e) => setForm(prev => ({ ...prev, password: e.target.value }))}
                  placeholder="可选"
                />
              </div>
            </div>
          </div>

          <DialogFooter>
            <Button variant="outline" onClick={() => setDialogOpen(false)} disabled={createProxy.isPending || updateProxy.isPending}>
              取消
            </Button>
            <Button onClick={handleSubmit} disabled={createProxy.isPending || updateProxy.isPending}>
              {createProxy.isPending || updateProxy.isPending ? '保存中...' : '保存'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
