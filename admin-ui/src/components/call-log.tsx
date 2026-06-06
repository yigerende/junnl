import { useState, useEffect } from 'react'
import { toast } from 'sonner'
import { RefreshCw, Trash2 } from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { useCallLogs, useClearCallLogs, useSetCallLogCapacity } from '@/hooks/use-call-log'

function formatTime(ms: number): string {
  const d = new Date(ms)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

export function CallLog() {
  const { data, isLoading, refetch } = useCallLogs(1000)
  const { mutate: clearLogs, isPending: isClearing } = useClearCallLogs()
  const { mutate: saveCapacity, isPending: isSavingCap } = useSetCallLogCapacity()
  const [capacityInput, setCapacityInput] = useState<number>(1000)

  useEffect(() => {
    if (data?.capacity) setCapacityInput(data.capacity)
  }, [data?.capacity])

  const logs = data?.logs ?? []

  const handleSaveCapacity = () => {
    saveCapacity(capacityInput, {
      onSuccess: (applied) => toast.success(`保留条数已设置为 ${applied}`),
      onError: (err) => toast.error(`设置失败: ${(err as Error).message}`),
    })
  }

  const handleClear = () => {
    clearLogs(undefined, {
      onSuccess: () => toast.success('调用日志已清空'),
      onError: (err) => toast.error(`清空失败: ${(err as Error).message}`),
    })
  }

  return (
    <>
      {/* 顶部操作栏 */}
      <div className="flex items-center justify-between mb-6">
        <div>
          <h1 className="text-xl font-semibold">调用日志</h1>
          <p className="text-sm text-muted-foreground">记录每次请求的下游模型与实际调用的上游模型（内存保留，重启清空）</p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={() => refetch()}>
            <RefreshCw className="h-4 w-4 mr-1" />
            刷新
          </Button>
          <Button variant="outline" size="sm" onClick={handleClear} disabled={isClearing} className="text-destructive hover:text-destructive">
            <Trash2 className="h-4 w-4 mr-1" />
            清空
          </Button>
        </div>
      </div>

      {/* 保留条数设置 */}
      <Card className="mb-6">
        <CardContent className="py-4">
          <div className="flex items-center gap-3">
            <span className="text-sm font-medium">保留条数</span>
            <input
              type="number"
              min={1}
              max={100000}
              value={capacityInput}
              onChange={e => setCapacityInput(Number(e.target.value))}
              className="w-32 h-9 rounded-md border border-input bg-background px-3 text-sm"
            />
            <Button size="sm" onClick={handleSaveCapacity} disabled={isSavingCap}>
              {isSavingCap ? '保存中...' : '保存'}
            </Button>
            <span className="text-xs text-muted-foreground">超过上限自动丢弃最早的记录，默认 1000</span>
          </div>
        </CardContent>
      </Card>

      {/* 日志表格 */}
      <Card>
        <CardHeader className="pb-3 flex-row items-center justify-between space-y-0">
          <CardTitle className="text-sm">最近调用（共 {logs.length} 条，最新在前）</CardTitle>
          <span className="text-xs text-muted-foreground">每 5 秒自动刷新</span>
        </CardHeader>
        <CardContent>
          {isLoading ? (
            <div className="flex items-center justify-center py-12">
              <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
            </div>
          ) : logs.length === 0 ? (
            <p className="text-sm text-muted-foreground py-6 text-center">暂无调用记录</p>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b text-muted-foreground text-left">
                    <th className="py-2 px-2 font-medium">时间</th>
                    <th className="py-2 px-2 font-medium">下游模型</th>
                    <th className="py-2 px-2 font-medium">上游模型</th>
                    <th className="py-2 px-2 font-medium">映射</th>
                    <th className="py-2 px-2 font-medium">流式</th>
                    <th className="py-2 px-2 font-medium">端点</th>
                  </tr>
                </thead>
                <tbody>
                  {logs.map((log, i) => (
                    <tr key={i} className="border-b border-border/50 hover:bg-muted/40">
                      <td className="py-1.5 px-2 whitespace-nowrap text-muted-foreground">{formatTime(log.timestampMs)}</td>
                      <td className="py-1.5 px-2 font-mono text-xs">{log.downstreamModel}</td>
                      <td className="py-1.5 px-2 font-mono text-xs">{log.upstreamModel}</td>
                      <td className="py-1.5 px-2">
                        {log.mapped
                          ? <span className="text-xs px-1.5 py-0.5 rounded bg-primary/10 text-primary">已映射</span>
                          : <span className="text-xs text-muted-foreground">—</span>}
                      </td>
                      <td className="py-1.5 px-2">
                        {log.stream
                          ? <span className="text-xs text-foreground">流式</span>
                          : <span className="text-xs text-muted-foreground">非流式</span>}
                      </td>
                      <td className="py-1.5 px-2 font-mono text-xs text-muted-foreground">{log.endpoint}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </CardContent>
      </Card>
    </>
  )
}
