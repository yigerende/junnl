import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { getCacheOptimizer, setCacheOptimizer } from '@/api/credentials'
import type { CacheOptimizerConfig } from '@/types/api'

export function useCacheOptimizer() {
  return useQuery({
    queryKey: ['cache-optimizer'],
    queryFn: getCacheOptimizer,
  })
}

export function useSetCacheOptimizer() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (config: CacheOptimizerConfig) => setCacheOptimizer(config),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['cache-optimizer'] })
    },
  })
}
