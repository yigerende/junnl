import { useState } from 'react'
import { toast } from 'sonner'
import { RefreshCw, ChevronUp, ChevronDown, Wallet, Trash2, Loader2, Zap, ShieldCheck } from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Switch } from '@/components/ui/switch'
import { Input } from '@/components/ui/input'
import { Checkbox } from '@/components/ui/checkbox'
import { Progress } from '@/components/ui/progress'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import type { CredentialStatusItem, BalanceResponse } from '@/types/api'
import { getCredentialBalance } from '@/api/credentials'
import {
  useSetDisabled,
  useSetPriority,
  useResetFailure,
  useDeleteCredential,
  useForceRefreshToken,
  useSetOverage,
} from '@/hooks/use-credentials'

interface CredentialCardProps {
  credential: CredentialStatusItem
  onViewBalance: (id: number) => void
  selected: boolean
  onToggleSelect: () => void
  balance: BalanceResponse | null
  loadingBalance: boolean
  onBalanceUpdate?: (balance: BalanceResponse) => void
}

function formatLastUsed(lastUsedAt: string | null): string {
  if (!lastUsedAt) return '从未使用'
  const date = new Date(lastUsedAt)
  const now = new Date()
  const diff = now.getTime() - date.getTime()
  if (diff < 0) return '刚刚'
  const seconds = Math.floor(diff / 1000)
  if (seconds < 60) return `${seconds} 秒前`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} 分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} 小时前`
  const days = Math.floor(hours / 24)
  return `${days} 天前`
}

export function CredentialCard({
  credential,
  onViewBalance,
  selected,
  onToggleSelect,
  balance,
  loadingBalance,
  onBalanceUpdate,
}: CredentialCardProps) {
  const [editingPriority, setEditingPriority] = useState(false)
  const [priorityValue, setPriorityValue] = useState(String(credential.priority))
  const [showDeleteDialog, setShowDeleteDialog] = useState(false)
  const [confirmingOverage, setConfirmingOverage] = useState(false)
  const [testingAlive, setTestingAlive] = useState(false)

  const setDisabled = useSetDisabled()
  const setPriority = useSetPriority()
  const resetFailure = useResetFailure()
  const deleteCredential = useDeleteCredential()
  const forceRefresh = useForceRefreshToken()
  const setOverage = useSetOverage()

  const handleToggleOverage = () => {
    if (!balance) return
    const enable = balance.overageStatus !== 'ENABLED'
    setConfirmingOverage(false)
    setOverage.mutate(
      { id: credential.id, enabled: enable },
      {
        onSuccess: (newBalance) => {
          // 用后端返回的最新余额同步到 dashboard，立即刷新卡片显示
          onBalanceUpdate?.(newBalance)
          toast.success(enable ? '已开启 Overages' : '已关闭 Overages')
        },
        onError: (err) => toast.error('设置失败: ' + (err as Error).message),
      }
    )
  }

  const handleTestAlive = async () => {
    setTestingAlive(true)
    try {
      // fresh=true 强制跳过缓存，拉上游最新，成功即代表凭据存活
      const balance = await getCredentialBalance(credential.id, true)
      onBalanceUpdate?.(balance)
      toast.success(`凭据 #${credential.id} 存活`)
    } catch (err) {
      toast.error(`凭据 #${credential.id} 测活失败: ${(err as Error).message}`)
    } finally {
      setTestingAlive(false)
    }
  }

  const handleToggleDisabled = () => {
    setDisabled.mutate(
      { id: credential.id, disabled: !credential.disabled },
      {
        onSuccess: (res) => {
          toast.success(res.message)
        },
        onError: (err) => {
          toast.error('操作失败: ' + (err as Error).message)
        },
      }
    )
  }

  const handlePriorityChange = () => {
    const newPriority = parseInt(priorityValue, 10)
    if (isNaN(newPriority) || newPriority < 0) {
      toast.error('优先级必须是非负整数')
      return
    }
    setPriority.mutate(
      { id: credential.id, priority: newPriority },
      {
        onSuccess: (res) => {
          toast.success(res.message)
          setEditingPriority(false)
        },
        onError: (err) => {
          toast.error('操作失败: ' + (err as Error).message)
        },
      }
    )
  }

  const handleReset = () => {
    resetFailure.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
      },
      onError: (err) => {
        toast.error('操作失败: ' + (err as Error).message)
      },
    })
  }

  const handleForceRefresh = () => {
    forceRefresh.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
      },
      onError: (err) => {
        toast.error('刷新失败: ' + (err as Error).message)
      },
    })
  }

  const handleDelete = () => {
    if (!credential.disabled) {
      toast.error('请先禁用凭据再删除')
      setShowDeleteDialog(false)
      return
    }

    deleteCredential.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
        setShowDeleteDialog(false)
      },
      onError: (err) => {
        toast.error('删除失败: ' + (err as Error).message)
      },
    })
  }

  return (
    <>
      <Card className={credential.isCurrent ? 'ring-2 ring-primary' : ''}>
        <CardHeader className="pb-2">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <Checkbox
                checked={selected}
                onCheckedChange={onToggleSelect}
              />
              <CardTitle className="text-lg flex items-center gap-2">
                {credential.email || `凭据 #${credential.id}`}
                {credential.isCurrent && (
                  <Badge variant="success">当前</Badge>
                )}
                {credential.disabled && (
                  <Badge variant="destructive">已禁用</Badge>
                )}
                {credential.disabled && credential.disabledReason && (
                  <Badge variant="outline">{credential.disabledReason}</Badge>
                )}
                {credential.authMethod && (
                  <Badge variant="secondary">
                    {credential.authMethod === 'api_key' ? 'API Key' :
                     credential.authMethod === 'idc' ? 'IdC' :
                     credential.authMethod === 'social' ? 'Social' :
                     credential.authMethod}
                  </Badge>
                )}
                {credential.endpoint && (
                  <Badge variant="outline">{credential.endpoint}</Badge>
                )}
              </CardTitle>
            </div>
            <div className="flex items-center gap-2">
              <span className="text-sm text-muted-foreground">启用</span>
              <Switch
                checked={!credential.disabled}
                onCheckedChange={handleToggleDisabled}
                disabled={setDisabled.isPending}
              />
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-4">
          {/* 信息网格 */}
          <div className="grid grid-cols-2 gap-4 text-sm">
            <div>
              <span className="text-muted-foreground">优先级：</span>
              {editingPriority ? (
                <div className="inline-flex items-center gap-1 ml-1">
                  <Input
                    type="number"
                    value={priorityValue}
                    onChange={(e) => setPriorityValue(e.target.value)}
                    className="w-16 h-7 text-sm"
                    min="0"
                  />
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-7 w-7 p-0"
                    onClick={handlePriorityChange}
                    disabled={setPriority.isPending}
                  >
                    ✓
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-7 w-7 p-0"
                    onClick={() => {
                      setEditingPriority(false)
                      setPriorityValue(String(credential.priority))
                    }}
                  >
                    ✕
                  </Button>
                </div>
              ) : (
                <span
                  className="font-medium cursor-pointer hover:underline ml-1"
                  onClick={() => setEditingPriority(true)}
                >
                  {credential.priority}
                  <span className="text-xs text-muted-foreground ml-1">(点击编辑)</span>
                </span>
              )}
            </div>
            <div>
              <span className="text-muted-foreground">失败次数：</span>
              <span className={credential.failureCount > 0 ? 'text-red-500 font-medium' : ''}>
                {credential.failureCount}
              </span>
            </div>
            <div>
              <span className="text-muted-foreground">刷新失败：</span>
              <span className={credential.refreshFailureCount > 0 ? 'text-red-500 font-medium' : ''}>
                {credential.refreshFailureCount}
              </span>
            </div>
            <div>
              <span className="text-muted-foreground">订阅等级：</span>
              <span className="font-medium">
                {loadingBalance ? (
                  <Loader2 className="inline w-3 h-3 animate-spin" />
                ) : balance?.subscriptionTitle || '未知'}
              </span>
            </div>
            <div>
              <span className="text-muted-foreground">成功次数：</span>
              <span className="font-medium">{credential.successCount}</span>
            </div>
            <div className="col-span-2">
              <span className="text-muted-foreground">最后调用：</span>
              <span className="font-medium">{formatLastUsed(credential.lastUsedAt)}</span>
            </div>
            {credential.maskedApiKey && (
              <div className="col-span-2">
                <span className="text-muted-foreground">API Key：</span>
                <span className="font-mono font-medium">{credential.maskedApiKey}</span>
              </div>
            )}
            <div className="col-span-2">
              {loadingBalance ? (
                <div className="text-sm text-muted-foreground">
                  <Loader2 className="inline w-3 h-3 animate-spin" /> 加载额度...
                </div>
              ) : balance ? (
                <div className="rounded-md border bg-muted/40 p-3 space-y-2">
                  {/* 已使用 / 总额度（开启超额时总额度含超额，否则为基础额度） */}
                  <div className="flex justify-between text-sm">
                    <span className="text-muted-foreground">已使用：</span>
                    <span className="font-medium">
                      {balance.currentUsage.toFixed(2)} / {balance.totalLimit.toFixed(2)}
                      <span className="text-xs text-muted-foreground ml-1">
                        ({(balance.totalLimit > 0 ? (balance.currentUsage / balance.totalLimit * 100) : 0).toFixed(1)}% 已用)
                      </span>
                    </span>
                  </div>
                  <Progress
                    value={balance.totalLimit > 0 ? Math.min(100, balance.currentUsage / balance.totalLimit * 100) : 0}
                  />
                  {/* Overages 状态行（始终显示当前开关状态） */}
                  {balance.overageStatus !== 'UNKNOWN' && (
                    <div className="flex justify-between text-sm">
                      <span className={balance.overageStatus === 'ENABLED' ? 'text-purple-600 font-medium' : 'text-muted-foreground'}>
                        Overages: {balance.overageStatus === 'ENABLED' ? 'Enabled' : 'Disabled'}
                      </span>
                      {balance.overageStatus === 'ENABLED' && (
                        <span className="text-xs text-muted-foreground">
                          总额度 = 基础 {balance.baseLimit.toLocaleString()} + 超额 {balance.overageCap.toLocaleString()}
                        </span>
                      )}
                    </div>
                  )}
                  {/* 超额用量行：仅在超额开启时显示 */}
                  {balance.overageStatus === 'ENABLED' && (
                    <div className="flex justify-between text-sm">
                      <span className="text-muted-foreground">超额用量：</span>
                      <span className="font-medium">
                        {balance.overageUsage.toFixed(0)} / {balance.overageCap.toLocaleString()}
                      </span>
                    </div>
                  )}
                </div>
              ) : (
                <div className="text-sm">
                  <span className="text-muted-foreground">剩余用量：</span>
                  <span className="text-muted-foreground ml-1">未知</span>
                </div>
              )}
            </div>
            {credential.hasProxy && (
              <div className="col-span-2">
                <span className="text-muted-foreground">代理：</span>
                <span className="font-medium">{credential.proxyUrl}</span>
              </div>
            )}
            {credential.hasProfileArn && (
              <div className="col-span-2">
                <Badge variant="secondary">有 Profile ARN</Badge>
              </div>
            )}
          </div>

          {/* 操作按钮 */}
          <div className="flex flex-wrap gap-2 pt-2 border-t">
            <Button
              size="sm"
              variant="outline"
              onClick={handleReset}
              disabled={resetFailure.isPending || (credential.failureCount === 0 && credential.refreshFailureCount === 0)}
            >
              <RefreshCw className="h-4 w-4 mr-1" />
              重置失败
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={handleForceRefresh}
              disabled={forceRefresh.isPending || credential.disabled || credential.authMethod === 'api_key'}
              title={credential.authMethod === 'api_key' ? 'API Key 凭据无需刷新 Token' : credential.disabled ? '已禁用的凭据无法刷新 Token' : '强制刷新 Token'}
            >
              <RefreshCw className={`h-4 w-4 mr-1 ${forceRefresh.isPending ? 'animate-spin' : ''}`} />
              刷新 Token
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                const newPriority = Math.max(0, credential.priority - 1)
                setPriority.mutate(
                  { id: credential.id, priority: newPriority },
                  {
                    onSuccess: (res) => toast.success(res.message),
                    onError: (err) => toast.error('操作失败: ' + (err as Error).message),
                  }
                )
              }}
              disabled={setPriority.isPending || credential.priority === 0}
            >
              <ChevronUp className="h-4 w-4 mr-1" />
              提高优先级
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                const newPriority = credential.priority + 1
                setPriority.mutate(
                  { id: credential.id, priority: newPriority },
                  {
                    onSuccess: (res) => toast.success(res.message),
                    onError: (err) => toast.error('操作失败: ' + (err as Error).message),
                  }
                )
              }}
              disabled={setPriority.isPending}
            >
              <ChevronDown className="h-4 w-4 mr-1" />
              降低优先级
            </Button>
            <Button
              size="sm"
              variant="default"
              onClick={() => onViewBalance(credential.id)}
            >
              <Wallet className="h-4 w-4 mr-1" />
              查看余额
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={handleTestAlive}
              disabled={testingAlive}
            >
              {testingAlive ? (
                <Loader2 className="h-4 w-4 mr-1 animate-spin" />
              ) : (
                <ShieldCheck className="h-4 w-4 mr-1" />
              )}
              单独测活
            </Button>
            {balance && balance.overageStatus !== 'UNKNOWN' && (
              <Button
                size="sm"
                variant="outline"
                onClick={() => setConfirmingOverage(true)}
                disabled={setOverage.isPending}
              >
                {setOverage.isPending ? (
                  <Loader2 className="h-4 w-4 mr-1 animate-spin" />
                ) : (
                  <Zap className="h-4 w-4 mr-1" />
                )}
                {balance.overageStatus === 'ENABLED' ? '关闭 Overages' : '开启 Overages'}
              </Button>
            )}
            <Button
              size="sm"
              variant="destructive"
              onClick={() => setShowDeleteDialog(true)}
              disabled={!credential.disabled}
              title={!credential.disabled ? '需要先禁用凭据才能删除' : undefined}
            >
              <Trash2 className="h-4 w-4 mr-1" />
              删除
            </Button>
          </div>
        </CardContent>
      </Card>

      {/* 删除确认对话框 */}
      <Dialog open={showDeleteDialog} onOpenChange={setShowDeleteDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>确认删除凭据</DialogTitle>
            <DialogDescription>
              您确定要删除凭据 #{credential.id} 吗？此操作无法撤销。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setShowDeleteDialog(false)}
              disabled={deleteCredential.isPending}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={deleteCredential.isPending || !credential.disabled}
            >
              确认删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Overages 二次确认 */}
      <Dialog open={confirmingOverage} onOpenChange={setConfirmingOverage}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {balance?.overageStatus === 'ENABLED' ? '确认关闭 Overages' : '确认开启 Overages'}
            </DialogTitle>
            <DialogDescription>
              {balance?.overageStatus === 'ENABLED'
                ? '关闭后，该账号超过基础额度将停止服务。此操作会修改上游账号的超额设置。'
                : '开启后，该账号超过基础额度会继续计费（产生真实费用）。此操作会修改上游账号的超额设置。'}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setConfirmingOverage(false)}
              disabled={setOverage.isPending}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={handleToggleOverage}
              disabled={setOverage.isPending}
            >
              {setOverage.isPending ? '处理中…' : '确认'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}
