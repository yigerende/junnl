import { useState, useEffect, useRef, useCallback } from 'react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Trash2 } from 'lucide-react'
import { storage } from '@/lib/storage'

/** 单条运行日志（与后端 LogRecord 对齐） */
interface LogRecord {
  seq: number
  ts: number
  level: string
  target: string
  message: string
  requestId?: string | null
  fields: Record<string, string>
}

const LEVEL_COLORS: Record<string, string> = {
  ERROR: 'text-red-500',
  WARN: 'text-amber-500',
  INFO: 'text-emerald-500',
  DEBUG: 'text-sky-500',
  TRACE: 'text-muted-foreground',
}

const MAX_ROWS = 5000

function formatTime(ms: number): string {
  const d = new Date(ms)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

export function RuntimeLog() {
  const [logs, setLogs] = useState<LogRecord[]>([])
  const [search, setSearch] = useState('')
  const [levelFilter, setLevelFilter] = useState('all')
  const [autoScroll, setAutoScroll] = useState(true)
  const [connected, setConnected] = useState(false)
  const bottomRef = useRef<HTMLDivElement>(null)
  const logsRef = useRef<LogRecord[]>([])

  // 追加日志并保持上限（用 ref 避免闭包拿到旧 state）
  const append = useCallback((rec: LogRecord) => {
    const next = [...logsRef.current, rec]
    if (next.length > MAX_ROWS) next.splice(0, next.length - MAX_ROWS)
    logsRef.current = next
    setLogs(next)
  }, [])

  // 建立 SSE 连接：用 fetch + ReadableStream 以便携带 x-api-key 头
  useEffect(() => {
    const apiKey = storage.getApiKey()
    const controller = new AbortController()
    let cancelled = false

    async function connect() {
      // 先拉取最近历史
      try {
        const resp = await fetch('/api/admin/logs?limit=5000', {
          headers: apiKey ? { 'x-api-key': apiKey } : {},
          signal: controller.signal,
        })
        if (resp.ok) {
          const data = await resp.json()
          if (!cancelled && Array.isArray(data.logs)) {
            logsRef.current = data.logs
            setLogs(data.logs)
          }
        }
      } catch {
        /* 历史拉取失败不致命，继续订阅实时流 */
      }

      // 订阅实时流
      try {
        const resp = await fetch('/api/admin/logs/stream', {
          headers: apiKey ? { 'x-api-key': apiKey } : {},
          signal: controller.signal,
        })
        if (!resp.ok || !resp.body) return
        setConnected(true)
        const reader = resp.body.getReader()
        const decoder = new TextDecoder()
        let buffer = ''
        while (!cancelled) {
          const { done, value } = await reader.read()
          if (done) break
          buffer += decoder.decode(value, { stream: true })
          // SSE 以空行分隔事件；每个事件含 "data: <json>" 行
          const parts = buffer.split('\n\n')
          buffer = parts.pop() ?? ''
          for (const part of parts) {
            for (const line of part.split('\n')) {
              const trimmed = line.startsWith('data:') ? line.slice(5).trim() : ''
              if (!trimmed) continue
              try {
                append(JSON.parse(trimmed) as LogRecord)
              } catch {
                /* 跳过无法解析的行 */
              }
            }
          }
        }
      } catch {
        /* 连接中断（含主动 abort），由 effect 清理 */
      } finally {
        if (!cancelled) setConnected(false)
      }
    }

    connect()
    return () => {
      cancelled = true
      controller.abort()
    }
  }, [append])

  // 自动滚动到底部
  useEffect(() => {
    if (autoScroll) bottomRef.current?.scrollIntoView({ behavior: 'auto' })
  }, [logs, autoScroll])

  const matches = (log: LogRecord): boolean => {
    if (levelFilter !== 'all' && log.level !== levelFilter) return false
    if (!search) return true
    const s = search.toLowerCase()
    if (log.message.toLowerCase().includes(s)) return true
    if (log.target.toLowerCase().includes(s)) return true
    if (log.requestId?.toLowerCase().includes(s)) return true
    for (const [k, v] of Object.entries(log.fields)) {
      if (k.toLowerCase().includes(s) || v.toLowerCase().includes(s)) return true
    }
    return false
  }

  const filtered = logs.filter(matches)

  const renderFields = (log: LogRecord) => {
    const parts: string[] = []
    if (log.requestId) parts.push(`request_id=${log.requestId}`)
    for (const [k, v] of Object.entries(log.fields)) {
      if (k === 'message') continue
      parts.push(`${k}=${v}`)
    }
    return parts.length ? ` {${parts.join(' ')}}` : ''
  }

  return (
    <>
      <div className="flex items-center justify-between mb-6">
        <div>
          <h1 className="text-xl font-semibold">运行日志</h1>
          <p className="text-sm text-muted-foreground">
            通过 SSE 实时接收服务端日志（内存保留最近 {MAX_ROWS} 条，重启清空）
          </p>
        </div>
        <div className="flex items-center gap-2">
          <span className={`text-xs ${connected ? 'text-emerald-500' : 'text-muted-foreground'}`}>
            {connected ? '● 实时' : '○ 未连接'}
          </span>
          <Button
            variant="outline"
            size="sm"
            onClick={() => { logsRef.current = []; setLogs([]) }}
            className="text-destructive hover:text-destructive"
          >
            <Trash2 className="h-4 w-4 mr-1" />
            清屏
          </Button>
        </div>
      </div>

      <Card className="mb-4">
        <CardContent className="py-3">
          <div className="flex items-center gap-3 flex-wrap">
            <input
              type="text"
              placeholder="搜索消息 / target / request_id / 字段..."
              value={search}
              onChange={e => setSearch(e.target.value)}
              className="flex-1 min-w-[200px] h-9 rounded-md border border-input bg-background px-3 text-sm"
            />
            <select
              value={levelFilter}
              onChange={e => setLevelFilter(e.target.value)}
              className="h-9 rounded-md border border-input bg-background px-3 text-sm"
            >
              <option value="all">全部级别</option>
              <option value="ERROR">ERROR</option>
              <option value="WARN">WARN</option>
              <option value="INFO">INFO</option>
              <option value="DEBUG">DEBUG</option>
            </select>
            <label className="flex items-center gap-1.5 text-sm text-muted-foreground">
              <input type="checkbox" checked={autoScroll} onChange={e => setAutoScroll(e.target.checked)} />
              自动滚动
            </label>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardContent className="py-3">
          <div className="text-xs text-muted-foreground mb-2">
            显示 {filtered.length} / {logs.length} 条
          </div>
          <div className="h-[calc(100vh-340px)] overflow-y-auto font-mono text-xs leading-relaxed bg-muted/30 rounded-md p-3">
            {filtered.length === 0 ? (
              <p className="text-muted-foreground text-center py-6">暂无日志</p>
            ) : (
              filtered.map(log => (
                <div key={log.seq} className="whitespace-pre-wrap break-all border-b border-border/30 py-0.5">
                  <span className="text-muted-foreground">{formatTime(log.ts)}</span>{' '}
                  <span className={LEVEL_COLORS[log.level] ?? 'text-foreground'}>{log.level.padEnd(5)}</span>{' '}
                  <span className="text-foreground">{log.message}</span>
                  <span className="text-muted-foreground">{renderFields(log)}</span>
                </div>
              ))
            )}
            <div ref={bottomRef} />
          </div>
        </CardContent>
      </Card>
    </>
  )
}

