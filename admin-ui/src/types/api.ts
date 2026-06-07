// 凭据状态响应
export interface CredentialsStatusResponse {
  total: number
  available: number
  currentId: number
  credentials: CredentialStatusItem[]
}

// 单个凭据状态
export interface CredentialStatusItem {
  id: number
  priority: number
  disabled: boolean
  failureCount: number
  isCurrent: boolean
  expiresAt: string | null
  authMethod: string | null
  provider?: string | null
  hasProfileArn: boolean
  email?: string
  refreshTokenHash?: string
  apiKeyHash?: string
  maskedApiKey?: string
  successCount: number
  lastUsedAt: string | null
  hasProxy: boolean
  proxyUrl?: string
  refreshFailureCount: number
  disabledReason?: string
  endpoint: string
}

// 余额响应
export interface BalanceResponse {
  id: number
  subscriptionTitle: string | null
  currentUsage: number
  usageLimit: number
  remaining: number
  usagePercentage: number
  nextResetAt: number | null
  overageStatus: string
  overageCapability: string | null
  baseLimit: number
  overageCap: number
  totalLimit: number
  overageUsage: number
}

// 成功响应
export interface SuccessResponse {
  success: boolean
  message: string
}

// 错误响应
export interface AdminErrorResponse {
  error: {
    type: string
    message: string
  }
}

// 请求类型
export interface SetDisabledRequest {
  disabled: boolean
}

export interface SetPriorityRequest {
  priority: number
}

// 添加凭据请求
export interface AddCredentialRequest {
  refreshToken?: string
  authMethod?: 'social' | 'idc' | 'api_key'
  provider?: string
  clientId?: string
  clientSecret?: string
  priority?: number
  authRegion?: string
  apiRegion?: string
  machineId?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  kiroApiKey?: string
  endpoint?: string
}

// 添加凭据响应
export interface AddCredentialResponse {
  success: boolean
  message: string
  credentialId: number
  email?: string
}

// 模拟缓存分段
export interface CacheSegment {
  min: number
  max: number
  weight: number
}

// 模拟缓存配置
export interface CacheOptimizerConfig {
  enabled: boolean
  enabledStream: boolean
  enabledNonStream: boolean
  enabledBuffered: boolean
  mode: 'passthrough' | 'zero' | 'cap' | 'random' | 'weighted'
  readMin: number
  readMax: number
  writeMin: number
  writeMax: number
  weightReadOnly: number
  weightWriteOnly: number
  weightReadWrite: number
  weightNone: number
  useSegmentWeights: boolean
  readSegments: [CacheSegment, CacheSegment, CacheSegment]
  writeSegments: [CacheSegment, CacheSegment, CacheSegment]
  rewriteOnlyWhenPresent: boolean
  keepRawBreakdown: boolean
  inputRandomMax: number
}

// 单条模型映射
export interface ModelMapping {
  alias: string
  target: string
  enabled: boolean
}

// 模型映射配置
export interface ModelMappingConfig {
  enabled: boolean
  hideMappedTargets: boolean
  mappings: ModelMapping[]
}

// 单条调用日志
export interface CallLogEntry {
  timestampMs: number
  downstreamModel: string
  upstreamModel: string
  stream: boolean
  endpoint: string
  mapped: boolean
}
