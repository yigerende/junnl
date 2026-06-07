import { useState, useEffect, useRef, useCallback, useMemo } from 'react'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Trash2, Download } from 'lucide-react'
import { storage } from '@/lib/storage'

/** 虚拟滚动：固定行高（px），只渲染可视区 + 上下缓冲行 */
const ROW_HEIGHT = 22
const OVERSCAN = 10

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

const MAX_ROWS = 1000

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
  const [logDir, setLogDir] = useState('')
  const [downloading, setDownloading] = useState(false)
  const logsRef = useRef<LogRecord[]>([])
  const pendingRef = useRef<LogRecord[]>([])
  const flushTimerRef = useRef<ReturnType<typeof setInterval> | null>(null)

  // 追加日志：先入缓冲，由定时器每 500ms 批量 flush 一次，
  // 避免每条 SSE 都触发整页重渲染导致卡死。
  // pending 也设上限：极端日志洪峰时只保留最近 MAX_ROWS 条，防止单次 flush 处理过大数组。
  const append = useCallback((rec: LogRecord) => {
    const pending = pendingRef.current
    pending.push(rec)
    if (pending.length > MAX_ROWS) {
      pending.splice(0, pending.length - MAX_ROWS)
    }
  }, [])

  // 批量 flush 定时器
  useEffect(() => {
    flushTimerRef.current = setInterval(() => {
      if (pendingRef.current.length === 0) return
      const next = [...logsRef.current, ...pendingRef.current]
      pendingRef.current = []
      if (next.length > MAX_ROWS) next.splice(0, next.length - MAX_ROWS)
      logsRef.current = next
      setLogs(next)
    }, 500)
    return () => {
      if (flushTimerRef.current) clearInterval(flushTimerRef.current)
    }
  }, [])

  // 建立 SSE 连接：用 fetch + ReadableStream 以便携带 x-api-key 头
  useEffect(() => {
    const apiKey = storage.getApiKey()
    const controller = new AbortController()
    let cancelled = false

    async function connect() {
      // 先拉取最近历史（限 300 条，避免首屏渲染过重）
      try {
        const resp = await fetch('/api/admin/logs?limit=300', {
          headers: apiKey ? { 'x-api-key': apiKey } : {},
          signal: controller.signal,
        })
        if (resp.ok) {
          const data = await resp.json()
          if (!cancelled && Array.isArray(data.logs)) {
            logsRef.current = data.logs.slice(-MAX_ROWS)
            setLogs(logsRef.current)
          }
        }
      } catch {
        /* 历史拉取失败不致命，继续订阅实时流 */
      }

      if (cancelled) return

      // 订阅实时流
      try {
        const resp = await fetch('/api/admin/logs/stream', {
          headers: apiKey ? { 'x-api-key': apiKey } : {},
          signal: controller.signal,
        })
        if (!resp.ok || !resp.body) {
          if (!cancelled) setConnected(false)
          return
        }
        if (!cancelled) setConnected(true)
        const reader = resp.body.getReader()
        const decoder = new TextDecoder()
        let buffer = ''
        while (!cancelled) {
          const { done, value } = await reader.read()
          if (done) break
          buffer += decoder.decode(value, { stream: true })
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

  // 拉取日志落盘信息（目录绝对路径），让用户知道日志文件到底落在哪
  useEffect(() => {
    const apiKey = storage.getApiKey()
    fetch('/api/admin/logs/info', {
      headers: apiKey ? { 'x-api-key': apiKey } : {},
    })
      .then(r => (r.ok ? r.json() : null))
      .then(data => { if (data?.dir) setLogDir(data.dir as string) })
      .catch(() => { /* 信息获取失败不致命 */ })
  }, [])

  // 一键导出当天落盘日志文件（带 x-api-key，故用 fetch -> blob 触发下载）
  const exportToday = useCallback(async () => {
    if (downloading) return
    setDownloading(true)
    try {
      const apiKey = storage.getApiKey()
      const resp = await fetch('/api/admin/logs/download', {
        headers: apiKey ? { 'x-api-key': apiKey } : {},
      })
      if (!resp.ok) {
        const msg = await resp.text().catch(() => '')
        alert(`导出失败：${resp.status} ${msg}`)
        return
      }
      const blob = await resp.blob()
      const today = new Date()
      const p = (n: number) => String(n).padStart(2, '0')
      const fname = `junnl-${today.getFullYear()}-${p(today.getMonth() + 1)}-${p(today.getDate())}.log`
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = fname
      document.body.appendChild(a)
      a.click()
      a.remove()
      URL.revokeObjectURL(url)
    } catch (e) {
      alert(`导出失败：${e instanceof Error ? e.message : String(e)}`)
    } finally {
      setDownloading(false)
    }
  }, [downloading])

  const matches = useCallback((log: LogRecord): boolean => {
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
  }, [levelFilter, search])

  // 过滤结果用 useMemo 缓存，只在 logs/搜索/级别变化时重算
  const filtered = useMemo(() => logs.filter(matches), [logs, matches])

  // ---- 虚拟滚动：只渲染可视区 + 上下缓冲的行 ----
  const scrollRef = useRef<HTMLDivElement>(null)
  const [scrollTop, setScrollTop] = useState(0)
  const [viewportH, setViewportH] = useState(600)
  const stickBottomRef = useRef(true) // 是否吸附在底部

  // 监听容器尺寸
  useEffect(() => {
    const el = scrollRef.current
    if (!el) return
    const ro = new ResizeObserver(() => setViewportH(el.clientHeight))
    ro.observe(el)
    setViewportH(el.clientHeight)
    return () => ro.disconnect()
  }, [])

  const onScroll = useCallback(() => {
    const el = scrollRef.current
    if (!el) return
    setScrollTop(el.scrollTop)
    // 距底部 <40px 视为吸附
    stickBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40
  }, [])

  // 新日志到达 + 开启自动滚动 + 当前吸底 → 滚到底
  useEffect(() => {
    if (!autoScroll) return
    const el = scrollRef.current
    if (el && stickBottomRef.current) {
      el.scrollTop = el.scrollHeight
    }
  }, [filtered.length, autoScroll])

  const total = filtered.length
  const startIdx = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN)
  const visibleCount = Math.ceil(viewportH / ROW_HEIGHT) + OVERSCAN * 2
  const endIdx = Math.min(total, startIdx + visibleCount)
  const visibleRows = filtered.slice(startIdx, endIdx)
  const padTop = startIdx * ROW_HEIGHT
  const padBottom = (total - endIdx) * ROW_HEIGHT

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
          {logDir && (
            <p className="text-xs text-muted-foreground mt-1">
              落盘目录：<span className="font-mono">{logDir}</span>（按天滚动，留存 7 天）
            </p>
          )}
        </div>
        <div className="flex items-center gap-2">
          <span className={`text-xs ${connected ? 'text-emerald-500' : 'text-muted-foreground'}`}>
            {connected ? '● 实时' : '○ 未连接'}
          </span>
          <Button
            variant="outline"
            size="sm"
            onClick={exportToday}
            disabled={downloading}
          >
            <Download className="h-4 w-4 mr-1" />
            {downloading ? '导出中...' : '导出当天'}
          </Button>
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
          <div
            ref={scrollRef}
            onScroll={onScroll}
            className="h-[calc(100vh-340px)] overflow-y-auto font-mono text-xs bg-muted/30 rounded-md p-3"
          >
            {total === 0 ? (
              <p className="text-muted-foreground text-center py-6">暂无日志</p>
            ) : (
              <div style={{ paddingTop: padTop, paddingBottom: padBottom }}>
                {visibleRows.map(log => (
                  <div
                    key={log.seq}
                    className="flex items-center gap-1 overflow-hidden whitespace-nowrap"
                    style={{ height: ROW_HEIGHT }}
                    title={`${log.message}${renderFields(log)}`}
                  >
                    <span className="text-muted-foreground shrink-0">{formatTime(log.ts)}</span>
                    <span className={`shrink-0 ${LEVEL_COLORS[log.level] ?? 'text-foreground'}`}>{log.level.padEnd(5)}</span>
                    <span className="text-foreground truncate">{log.message}</span>
                    <span className="text-muted-foreground truncate">{renderFields(log)}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        </CardContent>
      </Card>
    </>
  )
}

