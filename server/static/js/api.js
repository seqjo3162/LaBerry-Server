// /static/js/api.js

// Если функции log уже определены в app.js - используем их, иначе создаем локальные
const apiLog = typeof window !== 'undefined' && window._trace ? 
  (...args) => window._trace.add('API_LOG', args.join(' ')) : 
  (...args) => console.log(`[API:${new Date().toISOString()}]`, ...args);

const apiWarn = typeof window !== 'undefined' && window._trace ? 
  (...args) => window._trace.add('API_WARN', args.join(' ')) : 
  (...args) => console.warn(`[API:${new Date().toISOString()}]`, ...args);

const apiErr = typeof window !== 'undefined' && window._trace ? 
  (...args) => window._trace.add('API_ERROR', args.join(' ')) : 
  (...args) => console.error(`[API:${new Date().toISOString()}]`, ...args);

const apiTraceLog = [];
let apiRequestCounter = 0;

function apiTrace(step, data) {
  const entry = {
    id: ++apiRequestCounter,
    time: performance.now(),
    step,
    data
  };
  apiTraceLog.push(entry);
  
  // Логируем и в трейс и в консоль
  const message = `[API-TRACE:${entry.id}:${step}] ${JSON.stringify(data || '')}`;
  apiLog(message);
  
  // Также сохраняем в глобальный трейс
  if (window._trace) {
    window._trace.add(`API_${step}`, data);
  }
}

// Основная функция api
export async function api(path, opts = {}) {
  const requestId = ++apiRequestCounter;
  const startTime = performance.now();
  
  apiTrace('REQUEST_START', { 
    url: path, 
    requestId, 
    method: opts.method || 'GET',
    timestamp: new Date().toISOString()
  });

  try {
    const res = await fetch(path, {
      ...opts,
      headers: {
        "Authorization": localStorage.getItem("auth_token")
          ? "Bearer " + localStorage.getItem("auth_token")
          : undefined,
        "Content-Type": "application/json",
        ...opts.headers,
      },
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
      apiWarn("[API] Unauthorized – token invalid, clearing and reloading...");
      localStorage.removeItem("auth_token");
      window.location.href = "/";
      return [];
    }

    if (!res.ok) {
      const text = await res.text();
      apiTrace('REQUEST_ERROR', { 
        url: path, 
        requestId, 
        status: res.status,
        text: text.substring(0, 200) // Ограничиваем длину
      });
      apiErr(`[API] Request failed ${res.status}: ${text.substring(0, 100)}`);
      throw new Error(`API error ${res.status}: ${text.substring(0, 100)}`);
    }

    const data = await res.json();
    const totalTime = performance.now() - startTime;
    apiTrace('REQUEST_COMPLETE', { 
      url: path, 
      requestId, 
      time: totalTime.toFixed(2) + 'ms',
      dataSize: JSON.stringify(data).length,
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

// Вспомогательные функции
export const getFriends = () => api("/api/friends");
export const getActiveFriends = () => api("/api/friends/active");

// Экспорт для отладки
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

// Хук для отслеживания всех fetch запросов
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