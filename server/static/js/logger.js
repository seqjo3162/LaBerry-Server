// server/static/js/logger.js
const logger = {
  log: (message) => console.log(`[LOG] ${new Date().toISOString()}:`, message),
  warn: (message) => console.warn(`[WARN] ${new Date().toISOString()}:`, message),
  error: (message) => console.error(`[ERROR] ${new Date().toISOString()}:`, message)
};

export default logger;