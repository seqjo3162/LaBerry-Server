let API_DEBUG = false;
try {
  API_DEBUG = typeof window !== 'undefined' && (
    window.DEBUG_API === true || localStorage.getItem('lb_debug_api') === '1'
  );
} catch (_) {}

const apiLog = typeof window !== 'undefined' && window._trace ?
  (...args) => window._trace.add('API_LOG', args.join(' ')) :
  (API_DEBUG ? (...args) => console.log(`[API:${new Date().toISOString()}]`, ...args) : () => {});

const apiWarn = typeof window !== 'undefined' && window._trace ? 
  (...args) => window._trace.add('API_WARN', args.join(' ')) : 
  (...args) => console.warn(`[API:${new Date().toISOString()}]`, ...args);

const apiErr = typeof window !== 'undefined' && window._trace ? 
  (...args) => window._trace.add('API_ERROR', args.join(' ')) : 
  (...args) => console.error(`[API:${new Date().toISOString()}]`, ...args);

const apiTraceLog = [];
let apiRequestCounter = 0;

let refreshPromise = null;

async function refreshAccessToken() {
  if (refreshPromise) return refreshPromise;

  const refreshToken = localStorage.getItem('refresh_token');
  if (!refreshToken) return null;

  refreshPromise = (async () => {
    try {
      const res = await fetch('/api/auth/refresh', {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${refreshToken}`
        }
      });

      if (!res.ok) return null;
      const data = await res.json().catch(() => null);
      const next = data?.access_token;
      if (!next) return null;

      localStorage.setItem('auth_token', next);
      if (data?.refresh_token) {
        localStorage.setItem('refresh_token', data.refresh_token);
      }
      return next;
    } catch {
      return null;
    } finally {
      refreshPromise = null;
    }
  })();

  return refreshPromise;
}


export async function ensureSession(options = {}) {
  const { forceRefresh = false, redirectOnFail = false } = options;

  let token = localStorage.getItem('auth_token') || localStorage.getItem('token');
  if (!localStorage.getItem('auth_token') && token) {
    try { localStorage.setItem('auth_token', token); } catch (_) {}
    try { localStorage.removeItem('token'); } catch (_) {}
  }

  if (token && !forceRefresh) {
    return token;
  }

  const next = await refreshAccessToken();
  if (next) return next;

  if (redirectOnFail) {
    redirectToLogin();
  }
  return null;
}

function redirectToLogin() {
  try { localStorage.removeItem('auth_token'); } catch (_) {}
  try { localStorage.removeItem('refresh_token'); } catch (_) {}
  try { localStorage.removeItem('user_id'); } catch (_) {}
  try { sessionStorage.clear(); } catch (_) {}
  window.location.href = '/';
}

function apiTrace(step, data) {
  const entry = {
    id: ++apiRequestCounter,
    time: performance.now(),
    step,
    data
  };
  apiTraceLog.push(entry);
  
  const message = `[API-TRACE:${entry.id}:${step}] ${JSON.stringify(data || '')}`;
  apiLog(message);
  
  if (window._trace) {
    window._trace.add(`API_${step}`, data);
  }
}

// Встроенная проверка истечения JWT токена (из websocket-manager.js)
function _apiIsJwtExpired(token) {
  try {
    const parts = String(token || '').split('.');
    if (parts.length < 2) return false;
    const b64 = parts[1].replace(/-/g, '+').replace(/_/g, '/');
    const pad = b64.length % 4 ? '='.repeat(4 - (b64.length % 4)) : '';
    const json = atob(b64 + pad);
    const p = JSON.parse(json);
    const exp = p && typeof p.exp === 'number' ? p.exp : null;
    if (!exp) return false;
    const nowSec = Math.floor(Date.now() / 1000);
    return exp <= (nowSec + 15); // 15 сек skew
  } catch {
    return false;
  }
}

export async function api(path, opts = {}) {
  const requestId = ++apiRequestCounter;
  const startTime = performance.now();

  // внутренний флаг, чтобы не уезжал в fetch опции
  const optsForFetch = { ...opts };
  delete optsForFetch._didRefresh;
  
  apiTrace('REQUEST_START', {
    url: path,
    requestId,
    method: opts.method || 'GET',
    timestamp: new Date().toISOString()
  });

  try {
    let token = localStorage.getItem('auth_token') || localStorage.getItem('token');
    if (!localStorage.getItem('auth_token') && token) {
      try { localStorage.setItem('auth_token', token); } catch (_) {}
      try { localStorage.removeItem('token'); } catch (_) {}
    }

    // Предварительная проверка токена — если истёк, пробуем обновить ДО запроса
    if (token && _apiIsJwtExpired(token)) {
      apiWarn('[API] Token expired, refreshing before request...');
      const next = await refreshAccessToken();
      if (next) {
        token = next;
        apiWarn('[API] Token refreshed successfully');
      } else {
        apiWarn('[API] Token refresh failed – redirecting to login');
        redirectToLogin();
        const err = new Error('Unauthorized');
        err.status = 401;
        throw err;
      }
    }

    const headers = {
      ...(optsForFetch.headers || {}),
    };

    const hasContentType = Object.keys(headers).some(k => k.toLowerCase() === 'content-type');
    const isFormData = (typeof FormData !== 'undefined') && (optsForFetch.body instanceof FormData);

    // For JSON we set content-type automatically.
    // For FormData let the browser set the multipart boundary.
    if (!hasContentType && !isFormData) {
      headers['Content-Type'] = 'application/json';
    }
    if (token) {
      headers['Authorization'] = `Bearer ${token}`;
    }

    const res = await fetch(path, {
      ...optsForFetch,
      headers,
    });

    const responseTime = performance.now() - startTime;
    apiTrace('REQUEST_RESPONSE', { 
      url: path, 
      requestId, 
      status: res.status,
      time: responseTime.toFixed(2) + 'ms',
      headers: Object.fromEntries(res.headers.entries())
    });

    if (res.status === 401) {
      // Мягкое продление сессии: пытаемся обновить токен один раз и повторить запрос.
      // Это убирает вылет на / при долгом афк.
      const canRefresh = !!localStorage.getItem('refresh_token');
      const alreadyTried = !!opts._didRefresh;
      const isRefreshCall = path === '/api/auth/refresh';

      if (canRefresh && !alreadyTried && !isRefreshCall) {
        apiWarn('[API] 401 – trying refresh token...');
        const next = await refreshAccessToken();
        if (next) {
          apiWarn('[API] Token refreshed, retrying request...');
          return api(path, { ...opts, _didRefresh: true });
        }
      }

      apiWarn('[API] Unauthorized – redirecting to login');
      redirectToLogin();
      const err = new Error('Unauthorized');
      err.status = 401;
      throw err;
    }

    if (!res.ok) {
      const text = await res.text();
      let data = null;
      try { data = text ? JSON.parse(text) : null; } catch (_) {}
      apiTrace('REQUEST_ERROR', { 
        url: path, 
        requestId, 
        status: res.status,
        text: text.substring(0, 200)
      });
      apiErr(`[API] Request failed ${res.status}: ${text.substring(0, 100)}`);
      const err = new Error(`API error ${res.status}: ${text.substring(0, 100)}`);
      err.status = res.status;
      err.data = data;
      throw err;
    }

    const ct = (res.headers.get('content-type') || '').toLowerCase();
    const data = ct.includes('application/json') ? await res.json() : await res.text();
    const totalTime = performance.now() - startTime;
    apiTrace('REQUEST_COMPLETE', { 
      url: path, 
      requestId, 
      time: totalTime.toFixed(2) + 'ms',
      dataSize: (typeof data === 'string' ? data.length : JSON.stringify(data).length),
      hasData: !!data
    });

    return data;
  } catch (error) {
    const errorTime = performance.now() - startTime;
    apiTrace('REQUEST_FAILED', { 
      url: path, 
      requestId, 
      error: error.message,
      errorType: error.constructor.name,
      time: errorTime.toFixed(2) + 'ms'
    });
    apiErr(`[API] Request failed: ${error.message}`);
    throw error;
  }
}

export const getFriends = () => api("/api/friends");
export const getActiveFriends = () => api("/api/friends/active");

if (typeof window !== 'undefined') {
  window._apiTrace = {
    logs: apiTraceLog,
    dump: () => {
      console.group('=== API TRACE DUMP ===');
      apiTraceLog.forEach(log => {
        console.log(`#${log.id} ${log.step} @ ${log.time.toFixed(2)}ms`, log.data);
      });
      console.groupEnd();
    },
    getRequest: (id) => apiTraceLog.find(log => log.id === id),
    getLastRequest: () => apiTraceLog[apiTraceLog.length - 1]
  };
}

if (typeof window !== 'undefined' && window.DEBUG_API) {
  const originalFetch = window.fetch;
  window.fetch = function(...args) {
    const [url, options = {}] = args;
    const fetchId = ++apiRequestCounter;
    const startTime = performance.now();
    
    apiTrace('FETCH_START', {
      url: typeof url === 'string' ? url : url.url,
      method: options.method || 'GET',
      fetchId
    });
    
    return originalFetch.apply(this, args).then(response => {
      const time = performance.now() - startTime;
      apiTrace('FETCH_COMPLETE', {
        url: typeof url === 'string' ? url : url.url,
        status: response.status,
        time: time.toFixed(2) + 'ms',
        fetchId
      });
      return response;
    }).catch(error => {
      const time = performance.now() - startTime;
      apiTrace('FETCH_ERROR', {
        url: typeof url === 'string' ? url : url.url,
        error: error.message,
        time: time.toFixed(2) + 'ms',
        fetchId
      });
      throw error;
    });
  };
}
