/**
 * Structured Logging Module for RayDB
 * 
 * Provides a configurable logging abstraction that:
 * - Supports different log levels (debug, info, warn, error)
 * - Can be configured globally or per-component
 * - Avoids exposing sensitive file paths in production
 * - Supports structured logging with context objects
 */

export type LogLevel = 'debug' | 'info' | 'warn' | 'error' | 'silent';

export interface LogContext {
  [key: string]: unknown;
}

export interface LoggerConfig {
  /** Minimum log level to output */
  level: LogLevel;
  /** Whether to include timestamps */
  timestamps: boolean;
  /** Whether to redact file paths */
  redactPaths: boolean;
  /** Custom log handler (for testing or custom output) */
  handler?: (level: LogLevel, message: string, context?: LogContext) => void;
}

const LOG_LEVEL_PRIORITY: Record<LogLevel, number> = {
  debug: 0,
  info: 1,
  warn: 2,
  error: 3,
  silent: 4,
};

// Default configuration
let globalConfig: LoggerConfig = {
  level: 'warn',
  timestamps: false,
  redactPaths: false,
};

/**
 * Configure the global logger
 */
export function configureLogger(config: Partial<LoggerConfig>): void {
  globalConfig = { ...globalConfig, ...config };
}

/**
 * Get the current logger configuration
 */
export function getLoggerConfig(): LoggerConfig {
  return { ...globalConfig };
}

/**
 * Reset logger to default configuration
 */
export function resetLogger(): void {
  globalConfig = {
    level: 'warn',
    timestamps: false,
    redactPaths: false,
  };
}

/**
 * Redact file paths from a message
 */
function redactFilePaths(message: string): string {
  // Replace absolute file paths with [PATH]
  // Matches Unix paths like /foo/bar and Windows paths like C:\foo\bar
  return message
    .replace(/\/(?:[\w.-]+\/)+[\w.-]+/g, '[PATH]')
    .replace(/[A-Z]:\\(?:[\w.-]+\\)+[\w.-]+/gi, '[PATH]');
}

/**
 * Format a log message with optional context
 */
function formatMessage(
  level: LogLevel,
  message: string,
  context?: LogContext
): string {
  let formatted = message;

  // Redact paths if configured
  if (globalConfig.redactPaths) {
    formatted = redactFilePaths(formatted);
  }

  // Add timestamp if configured
  if (globalConfig.timestamps) {
    const timestamp = new Date().toISOString();
    formatted = `[${timestamp}] ${formatted}`;
  }

  // Add level prefix
  formatted = `[${level.toUpperCase()}] ${formatted}`;

  // Add context if present
  if (context && Object.keys(context).length > 0) {
    // Redact paths in context values too
    if (globalConfig.redactPaths) {
      const redactedContext: LogContext = {};
      for (const [key, value] of Object.entries(context)) {
        if (typeof value === 'string') {
          redactedContext[key] = redactFilePaths(value);
        } else {
          redactedContext[key] = value;
        }
      }
      formatted += ` ${JSON.stringify(redactedContext)}`;
    } else {
      formatted += ` ${JSON.stringify(context)}`;
    }
  }

  return formatted;
}

/**
 * Check if a log level should be output based on current config
 */
function shouldLog(level: LogLevel): boolean {
  return LOG_LEVEL_PRIORITY[level] >= LOG_LEVEL_PRIORITY[globalConfig.level];
}

/**
 * Internal log function
 */
function log(level: LogLevel, message: string, context?: LogContext): void {
  if (!shouldLog(level)) {
    return;
  }

  // Use custom handler if configured
  if (globalConfig.handler) {
    globalConfig.handler(level, message, context);
    return;
  }

  const formatted = formatMessage(level, message, context);

  // Output to appropriate console method
  switch (level) {
    case 'debug':
      console.debug(formatted);
      break;
    case 'info':
      console.info(formatted);
      break;
    case 'warn':
      console.warn(formatted);
      break;
    case 'error':
      console.error(formatted);
      break;
  }
}

/**
 * Logger object with methods for each log level
 */
export const logger = {
  debug(message: string, context?: LogContext): void {
    log('debug', message, context);
  },

  info(message: string, context?: LogContext): void {
    log('info', message, context);
  },

  warn(message: string, context?: LogContext): void {
    log('warn', message, context);
  },

  error(message: string, context?: LogContext): void {
    log('error', message, context);
  },
};

/**
 * Create a child logger with a specific component name
 * All logs from this logger will include the component name in context
 */
export function createLogger(component: string) {
  return {
    debug(message: string, context?: LogContext): void {
      log('debug', message, { component, ...context });
    },

    info(message: string, context?: LogContext): void {
      log('info', message, { component, ...context });
    },

    warn(message: string, context?: LogContext): void {
      log('warn', message, { component, ...context });
    },

    error(message: string, context?: LogContext): void {
      log('error', message, { component, ...context });
    },
  };
}

// Create component-specific loggers
export const gcLogger = createLogger('gc');
export const walLogger = createLogger('wal');
export const snapshotLogger = createLogger('snapshot');
export const lockLogger = createLogger('lock');
export const checkpointLogger = createLogger('checkpoint');
