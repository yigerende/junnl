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

// 格式化耗时（毫秒）：<1000 显示 ms，否则显示 s
function formatDuration(ms?: number | null): string {
  if (ms == null) return '—'
  if (ms < 1000) return `${ms}ms`
  return `${(ms / 1000).toFixed(2)}s`
}

// 格式化输入 token 数（千分位）
function formatTokens(n?: number | null): string {
  if (n == null) return '—'
  return n.toLocaleString()
}

export function CallLog() {
  const { data, isLoading, refetch } = useCallLogs(5000)
  const { mutate: clearLogs, isPending: isClearing } = useClearCallLogs()
  const { mutate: saveCapacity, isPending: isSavingCap } = useSetCallLogCapacity()
  const [capacityInput, setCapacityInput] = useState<number>(5000)

  useEffect(() => {
    if (data?.capacity) setCapacityInput(data.capacity)
  }, [data?.capacity])

  const logs = data?.logs ?? []

  // 筛选状态
  const [search, setSearch] = useState('')
  const [endpointFilter, setEndpointFilter] = useState('all')
  const [streamFilter, setStreamFilter] = useState('all')
  // 分页
  const [pageSize, setPageSize] = useState(20)
  const [currentPage, setCurrentPage] = useState(1)

  const filteredLogs = logs.filter((log) => {
    if (endpointFilter !== 'all' && log.endpoint !== endpointFilter) return false
    if (streamFilter === 'stream' && !log.stream) return false
    if (streamFilter === 'non-stream' && log.stream) return false
    if (search) {
      const s = search.toLowerCase()
      const hay = [
        log.downstreamModel,
        log.upstreamModel,
        log.endpoint,
        log.clientIp ?? '',
        log.clientHost ?? '',
        log.proxyHost ?? '',
        log.conversationId ?? '',
        log.credentialId != null ? `#${log.credentialId}` : '',
      ]
        .join(' ')
        .toLowerCase()
      if (!hay.includes(s)) return false
    }
    return true
  })

  // 分页计算
  const totalPages = Math.max(1, Math.ceil(filteredLogs.length / pageSize))
  const safePage = Math.min(currentPage, totalPages)
  const pagedLogs = filteredLogs.slice((safePage - 1) * pageSize, safePage * pageSize)

  // 筛选/每页条数变化时回到第一页
  useEffect(() => {
    setCurrentPage(1)
  }, [search, endpointFilter, streamFilter, pageSize])

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
            <span className="text-xs text-muted-foreground">超过上限自动丢弃最早的记录，默认 5000</span>
          </div>
        </CardContent>
      </Card>

      {/* 筛选区 */}
      <Card className="mb-4">
        <CardContent className="py-3">
          <div className="flex items-center gap-3 flex-wrap">
            <input
              type="text"
              placeholder="搜索 模型 / IP / 代理IP / 域名 / conversationId / 凭据#..."
              value={search}
              onChange={e => setSearch(e.target.value)}
              className="flex-1 min-w-[220px] h-9 rounded-md border border-input bg-background px-3 text-sm"
            />
            <select
              value={endpointFilter}
              onChange={e => setEndpointFilter(e.target.value)}
              className="h-9 rounded-md border border-input bg-background px-3 text-sm"
            >
              <option value="all">全部端点</option>
              <option value="/v1">/v1</option>
              <option value="/cc/v1">/cc/v1</option>
            </select>
            <select
              value={streamFilter}
              onChange={e => setStreamFilter(e.target.value)}
              className="h-9 rounded-md border border-input bg-background px-3 text-sm"
            >
              <option value="all">全部类型</option>
              <option value="stream">流式</option>
              <option value="non-stream">非流式</option>
            </select>
          </div>
        </CardContent>
      </Card>

      {/* 日志表格 */}
      <Card>
        <CardHeader className="pb-3 flex-row items-center justify-between space-y-0">
          <CardTitle className="text-sm">最近调用（共 {filteredLogs.length} / {logs.length} 条，最新在前）</CardTitle>
          <span className="text-xs text-muted-foreground">每 5 秒自动刷新</span>
        </CardHeader>
        <CardContent>
          {isLoading ? (
            <div className="flex items-center justify-center py-12">
              <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
            </div>
          ) : filteredLogs.length === 0 ? (
            <p className="text-sm text-muted-foreground py-6 text-center">暂无调用记录</p>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b text-muted-foreground text-left">
                    <th className="py-2 px-2 font-medium">时间</th>
                    <th className="py-2 px-2 font-medium">来源</th>
                    <th className="py-2 px-2 font-medium">凭据</th>
                    <th className="py-2 px-2 font-medium">代理IP</th>
                    <th className="py-2 px-2 font-medium">亲和</th>
                    <th className="py-2 px-2 font-medium">请求次数</th>
                    <th className="py-2 px-2 font-medium">下游模型</th>
                    <th className="py-2 px-2 font-medium">上游模型</th>
                    <th className="py-2 px-2 font-medium">映射</th>
                    <th className="py-2 px-2 font-medium">流式</th>
                    <th className="py-2 px-2 font-medium">输入token</th>
                    <th className="py-2 px-2 font-medium">下游输入token</th>
                    <th className="py-2 px-2 font-medium">首token</th>
                    <th className="py-2 px-2 font-medium">总耗时</th>
                    <th className="py-2 px-2 font-medium">端点</th>
                    <th className="py-2 px-2 font-medium">会话ID</th>
                    <th className="py-2 px-2 font-medium">结果</th>
                  </tr>
                </thead>
                <tbody>
                  {pagedLogs.map((log, i) => (
                    <tr key={i} className="border-b border-border/50 hover:bg-muted/40">
                      <td className="py-1.5 px-2 whitespace-nowrap text-muted-foreground">{formatTime(log.timestampMs)}</td>
                      <td className="py-1.5 px-2 text-xs whitespace-nowrap">
                        {log.clientIp ? <span className="font-mono">{log.clientIp}</span> : <span className="text-muted-foreground">—</span>}
                        {log.clientHost && <span className="text-muted-foreground ml-1">@{log.clientHost}</span>}
                      </td>
                      <td className="py-1.5 px-2 text-xs whitespace-nowrap">
                        {log.credentialId != null
                          ? <span className="px-1.5 py-0.5 rounded bg-primary/10 text-primary">#{log.credentialId}</span>
                          : <span className="text-muted-foreground">—</span>}
                      </td>
                      <td className="py-1.5 px-2 text-xs whitespace-nowrap">
                        {log.proxyHost
                          ? <span className="font-mono">{log.proxyHost}</span>
                          : <span className="text-muted-foreground">—</span>}
                      </td>
                      <td className="py-1.5 px-2 text-xs whitespace-nowrap">
                        {log.sessionAffinityHit
                          ? <span className="px-1.5 py-0.5 rounded bg-emerald-500/10 text-emerald-600">命中</span>
                          : <span className="text-muted-foreground">未命中</span>}
                      </td>
                      <td className="py-1.5 px-2 text-xs font-mono">
                        {log.credentialRequestCount != null ? log.credentialRequestCount.toLocaleString() : '—'}
                      </td>
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
                      <td className="py-1.5 px-2 text-xs font-mono whitespace-nowrap">
                        {log.inputTokens != null
                          ? <span className="text-foreground">{formatTokens(log.inputTokens)}</span>
                          : <span className="text-muted-foreground">—</span>}
                      </td>
                      <td className="py-1.5 px-2 text-xs font-mono whitespace-nowrap">
                        {log.reportedInputTokens != null
                          ? <span className="text-foreground">{formatTokens(log.reportedInputTokens)}</span>
                          : <span className="text-muted-foreground">—</span>}
                      </td>
                      <td className="py-1.5 px-2 text-xs font-mono whitespace-nowrap">
                        {log.firstTokenMs != null
                          ? <span className="text-foreground">{formatDuration(log.firstTokenMs)}</span>
                          : <span className="text-muted-foreground">—</span>}
                      </td>
                      <td className="py-1.5 px-2 text-xs font-mono whitespace-nowrap">
                        {log.totalDurationMs != null
                          ? <span className="text-foreground">{formatDuration(log.totalDurationMs)}</span>
                          : <span className="text-muted-foreground">—</span>}
                      </td>
                      <td className="py-1.5 px-2 font-mono text-xs text-muted-foreground">{log.endpoint}</td>
                      <td className="py-1.5 px-2 text-xs">
                        {log.conversationId
                          ? <span className="font-mono text-muted-foreground" title={log.conversationId}>{log.conversationId.length > 12 ? log.conversationId.slice(0, 12) + '…' : log.conversationId}</span>
                          : <span className="text-muted-foreground">—</span>}
                        {log.conversationIdSource && (
                          <span className={`ml-1 text-[10px] px-1 py-0.5 rounded ${log.conversationIdSource === 'random' ? 'bg-amber-500/10 text-amber-600' : 'bg-sky-500/10 text-sky-600'}`}>
                            {log.conversationIdSource}
                          </span>
                        )}
                      </td>
                      <td className="py-1.5 px-2">
                        {log.success
                          ? <span className="text-xs text-emerald-600">成功</span>
                          : <span className="text-xs text-red-500">失败</span>}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          {/* 分页控件 */}
          {filteredLogs.length > 0 && (
            <div className="flex items-center justify-between mt-4 flex-wrap gap-2">
              <div className="flex items-center gap-2 text-sm text-muted-foreground">
                <span>每页</span>
                <select
                  value={pageSize}
                  onChange={e => setPageSize(Number(e.target.value))}
                  className="h-8 rounded-md border border-input bg-background px-2 text-sm"
                >
                  <option value={10}>10</option>
                  <option value={20}>20</option>
                  <option value={50}>50</option>
                  <option value={100}>100</option>
                  <option value={200}>200</option>
                </select>
                <span>条</span>
              </div>
              <div className="flex items-center gap-2 text-sm">
                <span className="text-muted-foreground">
                  第 {safePage} / {totalPages} 页（共 {filteredLogs.length} 条）
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setCurrentPage(p => Math.max(1, p - 1))}
                  disabled={safePage <= 1}
                >
                  上一页
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setCurrentPage(p => Math.min(totalPages, p + 1))}
                  disabled={safePage >= totalPages}
                >
                  下一页
                </Button>
              </div>
            </div>
          )}
        </CardContent>
      </Card>
    </>
  )
}
