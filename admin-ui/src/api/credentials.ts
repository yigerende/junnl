import axios from 'axios'
import { storage } from '@/lib/storage'
import type {
  CredentialsStatusResponse,
  BalanceResponse,
  SuccessResponse,
  SetDisabledRequest,
  SetPriorityRequest,
  SetConcurrencyRequest,
  BatchSetConcurrencyRequest,
  AddCredentialRequest,
  AddCredentialResponse,
  CacheOptimizerConfig,
  ModelMappingConfig,
  CallLogEntry,
} from '@/types/api'

// 创建 axios 实例
const api = axios.create({
  baseURL: '/api/admin',
  headers: {
    'Content-Type': 'application/json',
  },
})

// 请求拦截器添加 API Key
api.interceptors.request.use((config) => {
  const apiKey = storage.getApiKey()
  if (apiKey) {
    config.headers['x-api-key'] = apiKey
  }
  return config
})

// 获取所有凭据状态
export async function getCredentials(): Promise<CredentialsStatusResponse> {
  const { data } = await api.get<CredentialsStatusResponse>('/credentials')
  return data
}

// 设置凭据禁用状态
export async function setCredentialDisabled(
  id: number,
  disabled: boolean
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${id}/disabled`,
    { disabled } as SetDisabledRequest
  )
  return data
}

// 设置凭据优先级
export async function setCredentialPriority(
  id: number,
  priority: number
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${id}/priority`,
    { priority } as SetPriorityRequest
  )
  return data
}

// 重置失败计数
export async function resetCredentialFailure(
  id: number
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/reset`)
  return data
}

// 设置单个凭据并发上限（0 = 不限制）
export async function setCredentialConcurrency(
  id: number,
  maxConcurrency: number
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/${id}/concurrency`,
    { maxConcurrency } as SetConcurrencyRequest
  )
  return data
}

// 批量设置凭据并发上限（0 = 不限制）
export async function batchSetConcurrency(
  ids: number[],
  maxConcurrency: number
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(
    `/credentials/concurrency/batch`,
    { ids, maxConcurrency } as BatchSetConcurrencyRequest
  )
  return data
}

// 强制刷新 Token
export async function forceRefreshToken(
  id: number
): Promise<SuccessResponse> {
  const { data } = await api.post<SuccessResponse>(`/credentials/${id}/refresh`)
  return data
}

// 获取凭据余额。fresh=true 时强制跳过缓存拉上游最新（单独测活）。
export async function getCredentialBalance(id: number, fresh = false): Promise<BalanceResponse> {
  const { data } = await api.get<BalanceResponse>(`/credentials/${id}/balance`, {
    params: fresh ? { fresh: true } : undefined,
  })
  return data
}

// 设置凭据超额开关（调上游 setUserPreference），返回最新余额
export async function setCredentialOverage(id: number, enabled: boolean): Promise<BalanceResponse> {
  const { data } = await api.post<BalanceResponse>(`/credentials/${id}/overage`, { enabled })
  return data
}

// 获取所有缓存的余额（只读，进页面立即展示，可能不是最新）
export async function getCachedBalances(): Promise<BalanceResponse[]> {
  const { data } = await api.get<{ balances: BalanceResponse[] }>('/balances/cached')
  return data.balances
}

// 添加新凭据
export async function addCredential(
  req: AddCredentialRequest
): Promise<AddCredentialResponse> {
  const { data } = await api.post<AddCredentialResponse>('/credentials', req)
  return data
}

// 删除凭据
export async function deleteCredential(id: number): Promise<SuccessResponse> {
  const { data } = await api.delete<SuccessResponse>(`/credentials/${id}`)
  return data
}

// 获取负载均衡模式
export async function getLoadBalancingMode(): Promise<{ mode: 'priority' | 'balanced' }> {
  const { data } = await api.get<{ mode: 'priority' | 'balanced' }>('/config/load-balancing')
  return data
}

// 设置负载均衡模式
export async function setLoadBalancingMode(mode: 'priority' | 'balanced'): Promise<{ mode: 'priority' | 'balanced' }> {
  const { data } = await api.put<{ mode: 'priority' | 'balanced' }>('/config/load-balancing', { mode })
  return data
}

// 获取模拟缓存配置
export async function getCacheOptimizer(): Promise<CacheOptimizerConfig> {
  const { data } = await api.get<{ config: CacheOptimizerConfig }>('/cache-optimizer')
  return data.config
}

// 更新模拟缓存配置
export async function setCacheOptimizer(config: CacheOptimizerConfig): Promise<CacheOptimizerConfig> {
  const { data } = await api.put<{ config: CacheOptimizerConfig }>('/cache-optimizer', config)
  return data.config
}

// 获取模型映射配置
export async function getModelMapping(): Promise<ModelMappingConfig> {
  const { data } = await api.get<{ config: ModelMappingConfig }>('/model-mapping')
  return data.config
}

// 更新模型映射配置
export async function setModelMapping(config: ModelMappingConfig): Promise<ModelMappingConfig> {
  const { data } = await api.put<{ config: ModelMappingConfig }>('/model-mapping', config)
  return data.config
}

// 拉取上游可用模型 ID 列表
export async function getAvailableModels(): Promise<string[]> {
  const { data } = await api.get<{ models: string[] }>('/available-models')
  return data.models
}

// 获取调用日志
export async function getCallLogs(limit = 1000): Promise<{ logs: CallLogEntry[]; capacity: number }> {
  const { data } = await api.get<{ logs: CallLogEntry[]; capacity: number }>('/call-logs', { params: { limit } })
  return data
}

// 清空调用日志
export async function clearCallLogs(): Promise<SuccessResponse> {
  const { data } = await api.delete<SuccessResponse>('/call-logs')
  return data
}

// 设置调用日志保留条数
export async function setCallLogCapacity(capacity: number): Promise<number> {
  const { data } = await api.put<{ capacity: number }>('/call-logs/capacity', { capacity })
  return data.capacity
}
