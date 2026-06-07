import { useState } from 'react'
import { toast } from 'sonner'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Progress } from '@/components/ui/progress'
import { Button } from '@/components/ui/button'
import { useCredentialBalance, useSetOverage } from '@/hooks/use-credentials'
import { parseError } from '@/lib/utils'

interface BalanceDialogProps {
  credentialId: number | null
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function BalanceDialog({ credentialId, open, onOpenChange }: BalanceDialogProps) {
  const { data: balance, isLoading, error } = useCredentialBalance(credentialId)
  const { mutate: setOverage, isPending: isTogglingOverage } = useSetOverage()
  const [confirming, setConfirming] = useState(false)

  const handleToggleOverage = (enable: boolean) => {
    if (credentialId == null) return
    setConfirming(false)
    setOverage(
      { id: credentialId, enabled: enable },
      {
        onSuccess: () => toast.success(enable ? '已开启超额' : '已关闭超额'),
        onError: (err) => toast.error(`设置失败：${parseError(err).title}`),
      },
    )
  }

  const formatDate = (timestamp: number | null) => {
    if (!timestamp) return '未知'
    return new Date(timestamp * 1000).toLocaleString('zh-CN')
  }

  const formatNumber = (num: number) => {
    return num.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 })
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>
            凭据 #{credentialId} 余额信息
          </DialogTitle>
        </DialogHeader>

        {isLoading && (
          <div className="flex items-center justify-center py-8">
            <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
          </div>
        )}

        {error && (() => {
          const parsed = parseError(error)
          return (
            <div className="py-6 space-y-3">
              <div className="flex items-center justify-center gap-2 text-red-500">
                <svg className="h-5 w-5" viewBox="0 0 20 20" fill="currentColor">
                  <path fillRule="evenodd" d="M10 18a8 8 0 100-16 8 8 0 000 16zM8.707 7.293a1 1 0 00-1.414 1.414L8.586 10l-1.293 1.293a1 1 0 101.414 1.414L10 11.414l1.293 1.293a1 1 0 001.414-1.414L11.414 10l1.293-1.293a1 1 0 00-1.414-1.414L10 8.586 8.707 7.293z" clipRule="evenodd" />
                </svg>
                <span className="font-medium">{parsed.title}</span>
              </div>
              {parsed.detail && (
                <div className="text-sm text-muted-foreground text-center px-4">
                  {parsed.detail}
                </div>
              )}
            </div>
          )
        })()}

        {balance && (
          <div className="space-y-4">
            {/* 订阅类型 */}
            <div className="text-center">
              <span className="text-lg font-semibold">
                {balance.subscriptionTitle || '未知订阅类型'}
              </span>
            </div>

            {/* 使用进度 */}
            <div className="space-y-2">
              <div className="flex justify-between text-sm">
                <span>已使用: ${formatNumber(balance.currentUsage)}</span>
                <span>限额: ${formatNumber(balance.usageLimit)}</span>
              </div>
              <Progress value={balance.usagePercentage} />
              <div className="text-center text-sm text-muted-foreground">
                {balance.usagePercentage.toFixed(1)}% 已使用
              </div>
            </div>

            {/* 详细信息 */}
            <div className="grid grid-cols-2 gap-4 pt-4 border-t text-sm">
              <div>
                <span className="text-muted-foreground">剩余额度：</span>
                <span className="font-medium text-green-600">
                  ${formatNumber(balance.remaining)}
                </span>
              </div>
              <div>
                <span className="text-muted-foreground">下次重置：</span>
                <span className="font-medium">
                  {formatDate(balance.nextResetAt)}
                </span>
              </div>
            </div>

            {/* 超额（Overages）*/}
            <div className="pt-4 border-t space-y-2">
              <div className="flex items-center justify-between">
                <span className="text-sm text-muted-foreground">Overages</span>
                <span
                  className={`text-sm font-medium ${
                    balance.overageStatus === 'ENABLED'
                      ? 'text-purple-600'
                      : 'text-muted-foreground'
                  }`}
                >
                  {balance.overageStatus === 'ENABLED'
                    ? 'Enabled'
                    : balance.overageStatus === 'DISABLED'
                      ? 'Disabled'
                      : '未知'}
                </span>
              </div>
              <div className="flex justify-between text-sm">
                <span className="text-muted-foreground">总额度</span>
                <span className="font-medium">
                  基础 {formatNumber(balance.baseLimit)} + 超额 {formatNumber(balance.overageCap)}
                  {' '}= {formatNumber(balance.totalLimit)}
                </span>
              </div>
              <div className="flex justify-between text-sm">
                <span className="text-muted-foreground">超额用量</span>
                <span className="font-medium">
                  {formatNumber(balance.overageUsage)} / {formatNumber(balance.overageCap)}
                </span>
              </div>

              {/* 开关：调上游真实计费设置，二次确认 */}
              {balance.overageStatus !== 'UNKNOWN' && (
                confirming ? (
                  <div className="rounded-md border border-amber-300 bg-amber-50 p-3 text-sm space-y-2">
                    <p className="text-amber-700">
                      {balance.overageStatus === 'ENABLED'
                        ? '确认关闭超额？关闭后该账号超过基础额度将停止服务。'
                        : '确认开启超额？开启后该账号超过基础额度会继续计费（产生真实费用）。'}
                    </p>
                    <div className="flex gap-2">
                      <Button
                        size="sm"
                        variant="destructive"
                        disabled={isTogglingOverage}
                        onClick={() => handleToggleOverage(balance.overageStatus !== 'ENABLED')}
                      >
                        {isTogglingOverage ? '处理中…' : '确认'}
                      </Button>
                      <Button size="sm" variant="outline" onClick={() => setConfirming(false)}>
                        取消
                      </Button>
                    </div>
                  </div>
                ) : (
                  <Button
                    size="sm"
                    variant="outline"
                    className="w-full mt-1"
                    disabled={isTogglingOverage}
                    onClick={() => setConfirming(true)}
                  >
                    {balance.overageStatus === 'ENABLED' ? '关闭 Overages' : '开启 Overages'}
                  </Button>
                )
              )}
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}
