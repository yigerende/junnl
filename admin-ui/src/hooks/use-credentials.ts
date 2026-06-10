import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  getCredentials,
  setCredentialDisabled,
  setCredentialPriority,
  setCredentialConcurrency,
  batchSetConcurrency,
  getProxies,
  createProxy,
  updateProxy,
  deleteProxy,
  testProxy,
  setCredentialProxies,
  batchSetCredentialProxies,
  resetCredentialFailure,
  forceRefreshToken,
  getCredentialBalance,
  setCredentialOverage,
  addCredential,
  deleteCredential,
  getLoadBalancingMode,
  setLoadBalancingMode,
} from '@/api/credentials'
import type { AddCredentialRequest, ProxyProfile } from '@/types/api'

// 查询凭据列表
// refreshSeconds: 自动刷新间隔（秒），<=0 表示关闭自动刷新
export function useCredentials(refreshSeconds = 3) {
  return useQuery({
    queryKey: ['credentials'],
    queryFn: getCredentials,
    refetchInterval: refreshSeconds > 0 ? refreshSeconds * 1000 : false,
  })
}

// 查询凭据余额
export function useCredentialBalance(id: number | null) {
  return useQuery({
    queryKey: ['credential-balance', id],
    queryFn: () => getCredentialBalance(id!),
    enabled: id !== null,
    retry: false, // 余额查询失败时不重试（避免重复请求被封禁的账号）
  })
}

// 设置超额开关（调上游），成功后写回余额缓存
export function useSetOverage() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) =>
      setCredentialOverage(id, enabled),
    onSuccess: (balance) => {
      // 用返回的最新余额直接更新缓存，立即反映新的超额状态
      queryClient.setQueryData(['credential-balance', balance.id], balance)
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 设置禁用状态
export function useSetDisabled() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, disabled }: { id: number; disabled: boolean }) =>
      setCredentialDisabled(id, disabled),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 设置优先级
export function useSetPriority() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, priority }: { id: number; priority: number }) =>
      setCredentialPriority(id, priority),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 设置单个凭据并发上限
export function useSetConcurrency() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, maxConcurrency }: { id: number; maxConcurrency: number }) =>
      setCredentialConcurrency(id, maxConcurrency),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 批量设置凭据并发上限
export function useBatchSetConcurrency() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ ids, maxConcurrency }: { ids: number[]; maxConcurrency: number }) =>
      batchSetConcurrency(ids, maxConcurrency),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

export function useProxies() {
  return useQuery({
    queryKey: ['proxies'],
    queryFn: getProxies,
  })
}

export function useCreateProxy() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: Omit<ProxyProfile, 'id'>) => createProxy(req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
    },
  })
}

export function useUpdateProxy() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, proxy }: { id: number; proxy: Omit<ProxyProfile, 'id'> }) =>
      updateProxy(id, proxy),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

export function useDeleteProxy() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => deleteProxy(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

export function useTestProxy() {
  return useMutation({
    mutationFn: (id: number) => testProxy(id),
  })
}

export function useSetCredentialProxies() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, proxyIds }: { id: number; proxyIds: number[] }) =>
      setCredentialProxies(id, proxyIds),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

export function useBatchSetCredentialProxies() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ ids, proxyIds }: { ids: number[]; proxyIds: number[] }) =>
      batchSetCredentialProxies(ids, proxyIds),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 重置失败计数
export function useResetFailure() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => resetCredentialFailure(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 强制刷新 Token
export function useForceRefreshToken() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => forceRefreshToken(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 添加新凭据
export function useAddCredential() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (req: AddCredentialRequest) => addCredential(req),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 删除凭据
export function useDeleteCredential() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: number) => deleteCredential(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
  })
}

// 获取负载均衡模式
export function useLoadBalancingMode() {
  return useQuery({
    queryKey: ['loadBalancingMode'],
    queryFn: getLoadBalancingMode,
  })
}

// 设置负载均衡模式
export function useSetLoadBalancingMode() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: setLoadBalancingMode,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['loadBalancingMode'] })
    },
  })
}
