import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { getCallLogs, clearCallLogs, setCallLogCapacity } from '@/api/credentials'

export function useCallLogs(limit = 1000) {
  return useQuery({
    queryKey: ['call-logs', limit],
    queryFn: () => getCallLogs(limit),
    refetchInterval: 5000, // 每 5 秒自动刷新
  })
}

export function useClearCallLogs() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: clearCallLogs,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['call-logs'] })
    },
  })
}

export function useSetCallLogCapacity() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (capacity: number) => setCallLogCapacity(capacity),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['call-logs'] })
    },
  })
}
