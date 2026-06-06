import { useState, useEffect } from 'react'
import { toast } from 'sonner'
import { RefreshCw, Trash2, Plus, Download } from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { useModelMapping, useSetModelMapping, useAvailableModels } from '@/hooks/use-model-mapping'
import type { ModelMappingConfig, ModelMapping as ModelMappingRow } from '@/types/api'

const DEFAULT_CONFIG: ModelMappingConfig = {
  enabled: false,
  hideMappedTargets: true,
  mappings: [],
}

export function ModelMapping() {
  const { data, isLoading, refetch } = useModelMapping()
  const { mutate: save, isPending: isSaving } = useSetModelMapping()
  const { data: availableModels, refetch: fetchModels, isFetching: isFetchingModels } = useAvailableModels()
  const [form, setForm] = useState<ModelMappingConfig>(DEFAULT_CONFIG)

  useEffect(() => {
    if (data) setForm(data)
  }, [data])

  const handleSave = () => {
    save(form, {
      onSuccess: (saved) => {
        setForm(saved)
        toast.success('模型映射已保存，下次请求生效')
      },
      onError: (err) => toast.error(`保存失败: ${(err as Error).message}`),
    })
  }

  const handleFetchModels = async () => {
    try {
      const res = await fetchModels()
      if (res.data && res.data.length > 0) {
        toast.success(`获取到 ${res.data.length} 个可用模型`)
      } else {
        toast.warning('上游未返回可用模型')
      }
    } catch (err) {
      toast.error(`获取失败: ${(err as Error).message}`)
    }
  }

  const updateField = <K extends keyof ModelMappingConfig>(key: K, value: ModelMappingConfig[K]) => {
    setForm(prev => ({ ...prev, [key]: value }))
  }

  const updateRow = (index: number, field: keyof ModelMappingRow, value: string | boolean) => {
    setForm(prev => {
      const mappings = [...prev.mappings]
      mappings[index] = { ...mappings[index], [field]: value }
      return { ...prev, mappings }
    })
  }

  const addRow = () => {
    setForm(prev => ({
      ...prev,
      mappings: [...prev.mappings, { alias: '', target: '', enabled: true }],
    }))
  }

  const removeRow = (index: number) => {
    setForm(prev => ({
      ...prev,
      mappings: prev.mappings.filter((_, i) => i !== index),
    }))
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-12">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
      </div>
    )
  }

  return (
    <>
      {/* 顶部操作栏 */}
      <div className="flex items-center justify-between mb-6">
        <div>
          <h1 className="text-xl font-semibold">模型映射</h1>
          <p className="text-sm text-muted-foreground">将请求模型映射到实际模型。左边是请求的模型，右边是发送到上游的实际模型。内部逻辑不变。</p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={() => refetch()}>
            <RefreshCw className="h-4 w-4 mr-1" />
            刷新
          </Button>
          <Button size="sm" onClick={handleSave} disabled={isSaving}>
            {isSaving ? '保存中...' : '保存'}
          </Button>
        </div>
      </div>

      {/* 开关区 */}
      <Card className="mb-6">
        <CardContent className="py-4 space-y-4">
          <label className="flex items-center justify-between">
            <div>
              <div className="font-medium">启用模型映射</div>
              <div className="text-sm text-muted-foreground">关闭时按原始模型名直接透传，开启时按下表替换</div>
            </div>
            <button
              type="button"
              role="switch"
              aria-checked={form.enabled}
              onClick={() => updateField('enabled', !form.enabled)}
              className={`relative inline-flex h-6 w-11 shrink-0 cursor-pointer rounded-full border-2 border-transparent transition-colors ${
                form.enabled ? 'bg-primary' : 'bg-muted'
              }`}
            >
              <span className={`pointer-events-none inline-block h-5 w-5 rounded-full bg-background shadow-lg ring-0 transition-transform ${
                form.enabled ? 'translate-x-5' : 'translate-x-0'
              }`} />
            </button>
          </label>

          <label className="flex items-center justify-between">
            <div>
              <div className="text-sm font-medium">模型列表显示 alias</div>
              <div className="text-xs text-muted-foreground">开启后 /v1/models 把已映射的实际模型显示为左边的请求模型名</div>
            </div>
            <button
              type="button"
              role="switch"
              aria-checked={form.hideMappedTargets}
              onClick={() => updateField('hideMappedTargets', !form.hideMappedTargets)}
              className={`relative inline-flex h-5 w-9 shrink-0 cursor-pointer rounded-full border-2 border-transparent transition-colors ${
                form.hideMappedTargets ? 'bg-primary' : 'bg-muted'
              }`}
            >
              <span className={`pointer-events-none inline-block h-4 w-4 rounded-full bg-background shadow-lg ring-0 transition-transform ${
                form.hideMappedTargets ? 'translate-x-4' : 'translate-x-0'
              }`} />
            </button>
          </label>
        </CardContent>
      </Card>

      {/* 映射表 */}
      <Card>
        <CardHeader className="pb-3 flex-row items-center justify-between space-y-0">
          <CardTitle className="text-sm">映射规则</CardTitle>
          <Button variant="outline" size="sm" onClick={handleFetchModels} disabled={isFetchingModels}>
            <Download className="h-4 w-4 mr-1" />
            {isFetchingModels ? '获取中...' : '获取模型列表'}
          </Button>
        </CardHeader>
        <CardContent className="space-y-3">
          {form.mappings.length === 0 && (
            <p className="text-sm text-muted-foreground py-2">暂无映射，点击下方「添加映射」新增一行</p>
          )}
          {form.mappings.map((row, index) => (
            <div key={index} className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={row.enabled}
                onChange={e => updateRow(index, 'enabled', e.target.checked)}
                className="h-4 w-4 shrink-0"
                title="启用该行"
              />
              <input
                type="text"
                value={row.alias}
                onChange={e => updateRow(index, 'alias', e.target.value)}
                placeholder="请求的模型 (如 claude-opus-4-8)"
                list="available-models-list"
                className="flex-1 h-9 rounded-md border border-input bg-background px-3 text-sm"
              />
              <span className="text-muted-foreground shrink-0">→</span>
              <input
                type="text"
                value={row.target}
                onChange={e => updateRow(index, 'target', e.target.value)}
                placeholder="实际模型 (如 claude-opus-4.8)"
                list="available-models-list"
                className="flex-1 h-9 rounded-md border border-input bg-background px-3 text-sm"
              />
              <Button variant="ghost" size="icon" onClick={() => removeRow(index)} className="shrink-0 text-destructive hover:text-destructive">
                <Trash2 className="h-4 w-4" />
              </Button>
            </div>
          ))}

          {/* 可用模型 datalist：alias/target 输入框均可下拉选择 */}
          <datalist id="available-models-list">
            {(availableModels ?? []).map(m => (
              <option key={m} value={m} />
            ))}
          </datalist>

          <Button variant="outline" className="w-full border-dashed" onClick={addRow}>
            <Plus className="h-4 w-4 mr-1" />
            添加映射
          </Button>

          {availableModels && availableModels.length > 0 && (
            <p className="text-xs text-muted-foreground">
              已获取 {availableModels.length} 个上游模型，输入框聚焦时可下拉选择。
            </p>
          )}
        </CardContent>
      </Card>
    </>
  )
}
