import {
  buildCacheKey,
  cacheControlHeader,
  type CacheStatus,
  withCache,
} from './cache.js'
import { ConvertError } from './errors.js'
import { fetchPosts, type ContextMode, type FetchSource, type RepliesMode } from './tweet-fetch.js'
import { renderThreadMarkdown, type UserinfoLevel } from './markdown.js'

export type OutputFormat = 'markdown' | 'obsidian' | 'json'

export { ConvertError }

export interface ConvertInput {
  url?: string | null
  handle?: string | null
  id?: string | null
  format?: string | null
  thread?: string | null
  userinfo?: string | null
  nocache?: boolean | string | null
  full?: boolean | string | null
  context?: string | null
  replies?: string | null
}

export interface ConvertSuccess {
  body: string
  warnings: string[]
  canonicalUrl: string
  format: OutputFormat
  postCount: number
  source: FetchSource
  cache: CacheStatus
  posts: FxTweet[]
  compact: boolean
}

import type { FxTweet } from './fxtwitter.js'

const ALLOWED_HOSTS = new Set([
  'x.com',
  'twitter.com',
  'www.twitter.com',
  'mobile.twitter.com',
])
const STATUS_PATH = /^\/([A-Za-z0-9_]{1,15})\/status\/(\d{1,25})\/?$/

export function parseStatusUrl(raw: string): { handle: string; id: string; canonicalUrl: string } {
  let parsed: URL
  try {
    parsed = new URL(raw.trim())
  } catch {
    throw new ConvertError(400, 'Invalid URL. Provide a public X/Twitter status URL.', 'invalid_url')
  }

  const host = parsed.hostname.replace(/^www\./, '')
  const isAllowed = ALLOWED_HOSTS.has(host) && ['https:', 'http:'].includes(parsed.protocol)
    && !parsed.username && !parsed.password && !parsed.port

  if (!isAllowed) {
    throw new ConvertError(
      400,
      'Only x.com or twitter.com status URLs are supported.',
      'unsupported_host',
    )
  }

  const match = STATUS_PATH.exec(parsed.pathname)
  if (!match) {
    throw new ConvertError(
      400,
      'URL must be a status permalink like https://x.com/handle/status/1234567890.',
      'invalid_path',
    )
  }

  const handle = match[1]
  const id = match[2]
  return {
    handle,
    id,
    canonicalUrl: `https://x.com/${handle}/status/${id}`,
  }
}

export function resolveTarget(input: ConvertInput): { canonicalUrl: string; handle: string; id: string } {
  if (input.url) {
    return parseStatusUrl(input.url)
  }

  if (input.handle && input.id) {
    const handle = input.handle.replace(/^@/, '')
    const id = input.id
    if (!/^[A-Za-z0-9_]{1,15}$/.test(handle) || !/^\d{1,25}$/.test(id)) {
      throw new ConvertError(400, 'Missing or invalid handle/status id.', 'invalid_params')
    }
    return {
      handle,
      id,
      canonicalUrl: `https://x.com/${handle}/status/${id}`,
    }
  }

  throw new ConvertError(400, 'Missing required `url` query parameter.', 'missing_url')
}

function parseFormat(raw: string | null | undefined): OutputFormat {
  if (!raw || raw === 'markdown') return 'markdown'
  if (raw === 'obsidian') return 'obsidian'
  if (raw === 'json') return 'json'
  throw new ConvertError(400, '`format` must be `markdown`, `obsidian`, or `json`.', 'invalid_format')
}

const DEFAULT_THREAD = 'full'

function canonicalThreadCacheValue(raw: string | null | undefined): string {
  if (raw === 'off') return 'off'
  if (!raw || raw === 'full' || raw === 'conversation') return DEFAULT_THREAD
  return raw
}

function parseThread(raw: string | null | undefined): { mode: 'off' | 'full'; limit: number } {
  if (raw === 'off') return { mode: 'off', limit: 1 }

  if (!raw || raw === 'full' || raw === 'conversation') return { mode: 'full', limit: 100 }

  const n = Number(raw)
  if (Number.isInteger(n) && n >= 2 && n <= 100) {
    return { mode: 'full', limit: n }
  }

  throw new ConvertError(
    400,
    '`thread` must be `off`, `full`, `conversation`, or a number from 2 to 100.',
    'invalid_thread',
  )
}

function parseUserinfo(raw: string | null | undefined): UserinfoLevel {
  if (!raw || raw === 'off') return 'off'
  if (raw === 'author') return 'author'
  if (raw === 'all') return 'all'
  throw new ConvertError(400, '`userinfo` must be `off`, `author`, or `all`.', 'invalid_userinfo')
}

function parseBoolean(raw: string | boolean | null | undefined): boolean {
  if (raw === true) return true
  if (raw === false || raw == null) return false
  return raw === '1' || raw === 'true' || raw === 'yes'
}

function parseContext(raw: string | null | undefined): ContextMode {
  if (!raw || raw === 'full') return 'full'
  if (raw === 'thread') return 'thread'
  throw new ConvertError(400, '`context` must be `full` or `thread`.', 'invalid_context')
}

function parseReplies(raw: string | null | undefined): RepliesMode {
  if (!raw || raw === 'top') return 'top'
  if (raw === 'recent' || raw === 'off') return raw
  throw new ConvertError(400, '`replies` must be `top`, `recent`, or `off`.', 'invalid_replies')
}

function withSourceUrls(tweet: FxTweet, fallback?: string): FxTweet {
  const handle = tweet.author?.screen_name
  const url = tweet.url ?? (handle && tweet.id ? `https://x.com/${handle}/status/${tweet.id}` : fallback)
  return { ...tweet, url, quote: tweet.quote ? withSourceUrls(tweet.quote) : undefined }
}

function limitPostsByRole(posts: FxTweet[], requestedId: string, limit: number): FxTweet[] {
  if (posts.length <= limit) return posts
  const focalIndex = posts.findIndex((post) => post.id === requestedId)
  const candidates = posts.map((post, index) => ({ post, index, priority:
    post.id === requestedId ? 0 : post.context === 'parent' || post.context === 'thread' ? 1 : 2,
  }))
  candidates.sort((a, b) => {
    if (a.priority !== b.priority) return a.priority - b.priority
    // Closest parent first; continuations and ranked replies retain provider order.
    if (a.post.context === 'parent' && b.post.context === 'parent') {
      return Math.abs(focalIndex - a.index) - Math.abs(focalIndex - b.index)
    }
    return a.index - b.index
  })
  const selected = new Set(candidates.slice(0, limit).map(({ index }) => index))
  return posts.filter((_post, index) => selected.has(index))
}

type ConvertPayload = Omit<ConvertSuccess, 'cache'>

async function convertTweetUncached(
  format: OutputFormat,
  thread: { mode: 'off' | 'full'; limit: number },
  userinfo: UserinfoLevel,
  canonicalUrl: string,
  handle: string,
  id: string,
  compact: boolean,
  context: ContextMode,
  replies: RepliesMode,
  signal?: AbortSignal,
): Promise<ConvertPayload> {
  const warnings: string[] = []

  const { tweets, source } = await fetchPosts(handle, id, thread.mode, context, replies, signal)

  let posts = tweets
  if (thread.mode === 'full' && posts.length > thread.limit) {
    posts = limitPostsByRole(posts, id, thread.limit)
    warnings.push(`Thread truncated to ${thread.limit} posts.`)
  }
  posts = posts.map((post) =>
    withSourceUrls(post, post.id === id || posts.length === 1 ? canonicalUrl : undefined),
  )

  if (source !== 'fxtwitter') {
    warnings.push(
      `Fetched via ${source} fallback — threads, full articles, and quotes may be limited.`,
    )
  }

  const body = renderThreadMarkdown(posts, {
    format: format === 'json' ? 'markdown' : format,
    userinfo,
    canonicalUrl,
    compact: compact && format !== 'obsidian',
  })

  return {
    body,
    warnings,
    canonicalUrl,
    format,
    postCount: posts.length,
    source,
    posts,
    compact: compact && format !== 'obsidian',
  }
}

export async function convertTweet(input: ConvertInput, signal?: AbortSignal): Promise<ConvertSuccess> {
  const format = parseFormat(input.format)
  const thread = parseThread(input.thread)
  const userinfo = parseUserinfo(input.userinfo)
  const nocache = parseBoolean(input.nocache)
  const compact = !parseBoolean(input.full)
  const context = parseContext(input.context)
  const replies = parseReplies(input.replies)
  const { canonicalUrl, handle, id } = resolveTarget(input)

  const cacheKey = buildCacheKey({
    v: 5,
    id,
    handle: handle.toLowerCase(),
    format,
    thread: canonicalThreadCacheValue(input.thread),
    userinfo: input.userinfo ?? 'off',
    compact: compact ? '1' : '0',
    context,
    replies,
  })

  const { value, status } = await withCache(cacheKey, nocache, async () =>
    convertTweetUncached(format, thread, userinfo, canonicalUrl, handle, id, compact, context, replies, signal),
  )

  return { ...value, cache: status }
}

export function markdownResponse(result: ConvertSuccess, asJson = false): {
  status: number
  headers: Record<string, string>
  body: string
} {
  const sharedHeaders: Record<string, string> = {
    Vary: 'Accept, User-Agent',
    'X-Converter': 'nanocodex-x',
    'X-Source': result.source,
    'X-Post-Count': String(result.postCount),
    'X-Warnings': String(result.warnings.length),
    'X-Cache': result.cache.toUpperCase(),
  }

  if (result.cache !== 'bypass') {
    sharedHeaders['Cache-Control'] = cacheControlHeader()
  }

  if (asJson) {
    return {
      status: 200,
      headers: { 'Content-Type': 'application/json; charset=utf-8', ...sharedHeaders },
      body: JSON.stringify({
        format: result.format,
        url: result.canonicalUrl,
        markdown: result.body,
        posts: result.posts,
        compact: result.compact,
        warnings: result.warnings,
        postCount: result.postCount,
        source: result.source,
        cache: result.cache,
      }),
    }
  }

  return {
    status: 200,
    headers: {
      'Content-Type': 'text/markdown; charset=utf-8',
      ...sharedHeaders,
    },
    body: result.body,
  }
}
