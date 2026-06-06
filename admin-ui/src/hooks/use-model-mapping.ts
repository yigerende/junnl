import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { getModelMapping, setModelMapping, getAvailableModels } from '@/api/credentials'
import type { ModelMappingConfig } from '@/types/api'

export function useModelMapping() {
  return useQuery({
    queryKey: ['model-mapping'],
    queryFn: getModelMapping,
  })
}

export function useSetModelMapping() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (config: ModelMappingConfig) => setModelMapping(config),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['model-mapping'] })
    },
  })
}

export function useAvailableModels() {
  return useQuery({
    queryKey: ['available-models'],
    queryFn: getAvailableModels,
    enabled: false, // 手动触发（点击「获取模型列表」）
    staleTime: 60_000,
  })
}
