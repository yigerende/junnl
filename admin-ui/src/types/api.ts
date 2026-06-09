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
  maxConcurrency: number
  activeConcurrency: number
  waitingConcurrency: number
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

export interface SetConcurrencyRequest {
  maxConcurrency: number
}

export interface BatchSetConcurrencyRequest {
  ids: number[]
  maxConcurrency: number
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
  maxConcurrency?: number
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

// 输入放大分档
export interface InputScaleSegment {
  min: number
  max: number
  readMultiplier: number
  writeMultiplier: number
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
  // 探活豁免
  probeBypassMaxInputTokens: number | null
  probeBypassStream: boolean
  probeBypassNonStream: boolean
  probeBypassBuffered: boolean
  // 输入放大
  inputScaleEnabled: boolean
  inputScaleMaxRead: number | null
  inputScaleMaxWrite: number | null
  inputScaleSegments: InputScaleSegment[]
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
  clientIp?: string | null
  clientHost?: string | null
  credentialId?: number | null
  credentialRequestCount?: number | null
  conversationId?: string | null
  conversationIdSource?: string | null
  sessionAffinityHit?: boolean
  success?: boolean
  /** 首 token 耗时（毫秒）：请求进入到上游首字节到达 */
  firstTokenMs?: number | null
  /** 总耗时（毫秒）：请求进入到响应流读完 */
  totalDurationMs?: number | null
}
