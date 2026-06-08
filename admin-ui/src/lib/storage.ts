const API_KEY_STORAGE_KEY = 'adminApiKey'
const REFRESH_INTERVAL_KEY = 'credentialsRefreshSeconds'

export const storage = {
  getApiKey: () => localStorage.getItem(API_KEY_STORAGE_KEY),
  setApiKey: (key: string) => localStorage.setItem(API_KEY_STORAGE_KEY, key),
  removeApiKey: () => localStorage.removeItem(API_KEY_STORAGE_KEY),

  // 凭据列表自动刷新间隔（秒）。默认 3 秒，0 表示关闭自动刷新。
  getRefreshSeconds: (): number => {
    const raw = localStorage.getItem(REFRESH_INTERVAL_KEY)
    if (raw === null) return 3
    const n = parseInt(raw, 10)
    return Number.isFinite(n) && n >= 0 ? n : 3
  },
  setRefreshSeconds: (seconds: number) =>
    localStorage.setItem(REFRESH_INTERVAL_KEY, String(seconds)),
}
