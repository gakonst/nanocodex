import { ConvertError } from './errors.js'
import {
  fetchFxConversationReplies,
  fetchFxFullThread,
  fetchFxStatus,
  type FxReplyRanking,
  type FxTweet,
} from './fxtwitter.js'
import { fetchSyndicationStatus } from './syndication.js'

export type FetchSource = 'fxtwitter' | 'syndication'

export interface FetchResult {
  tweets: FxTweet[]
  source: FetchSource
}

export type ContextMode = 'full' | 'thread'
export type RepliesMode = 'top' | 'recent' | 'off'

function annotateAndDedupe(thread: FxTweet[], requestedId: string, replies: FxTweet[]): FxTweet[] {
  const requestedIndex = thread.findIndex((tweet) => tweet.id === requestedId)
  const seen = new Set<string>()
  const output: FxTweet[] = []
  const add = (tweet: FxTweet, context: FxTweet['context']) => {
    if (tweet.id && seen.has(tweet.id)) return
    if (tweet.id) seen.add(tweet.id)
    output.push({ ...tweet, context })
  }

  thread.forEach((tweet, index) => {
    const context = tweet.id === requestedId
      ? 'post'
      : requestedIndex >= 0 && index < requestedIndex ? 'parent' : 'thread'
    add(tweet, context)
  })
  replies.forEach((tweet) => add(tweet, 'reply'))
  return output
}

function focalAuthorThread(thread: FxTweet[], requestedId: string, requestedHandle: string): FxTweet[] {
  const focal = thread.find((tweet) => tweet.id === requestedId)
  const authorId = focal?.author?.id
  const handle = focal?.author?.screen_name?.toLowerCase() ?? requestedHandle.toLowerCase()
  return thread.filter((tweet) => {
    if (tweet.id === requestedId) return true
    if (authorId) return tweet.author?.id === authorId
    return Boolean(handle && tweet.author?.screen_name?.toLowerCase() === handle)
  })
}

function isHardNotFound(error: unknown): boolean {
  return error instanceof ConvertError && ['private_tweet', 'rate_limited'].includes(error.code ?? '')
}

async function fetchStatusWithFallback(handle: string, id: string, signal?: AbortSignal): Promise<FetchResult> {
  const attempts: Array<() => Promise<FetchResult>> = [
    async () => ({ tweets: [await fetchFxStatus(id, signal)], source: 'fxtwitter' }),
    async () => ({ tweets: [await fetchSyndicationStatus(handle, id, signal)], source: 'syndication' }),
  ]

  let lastError: unknown

  for (const attempt of attempts) {
    try {
      return await attempt()
    } catch (error) {
      signal?.throwIfAborted()
      lastError = error
      if (isHardNotFound(error)) throw error
    }
  }

  throw lastError instanceof ConvertError
    ? lastError
    : new ConvertError(502, 'All fetch providers failed.', 'all_providers_failed')
}

export async function fetchPosts(
  handle: string,
  id: string,
  threadMode: 'off' | 'full',
  contextMode: ContextMode = 'full',
  repliesMode: RepliesMode = 'top',
  signal?: AbortSignal,
): Promise<FetchResult> {
  if (threadMode === 'off') {
    const result = await fetchStatusWithFallback(handle, id, signal)
    return { ...result, tweets: annotateAndDedupe(result.tweets, id, []) }
  }

  try {
    const assembledThread = await fetchFxFullThread(id, signal)
    const thread = contextMode === 'thread'
      ? focalAuthorThread(assembledThread, id, handle)
      : assembledThread
    if (repliesMode === 'off' || contextMode === 'thread') {
      return { tweets: annotateAndDedupe(thread, id, []), source: 'fxtwitter' }
    }

    let replies: FxTweet[] = []
    try {
      const ranking: FxReplyRanking = repliesMode === 'recent' ? 'recency' : 'likes'
      replies = (await fetchFxConversationReplies(id, ranking, 10, signal)) ?? []
    } catch {
      signal?.throwIfAborted()
      // Reply context is additive; preserve the existing thread/provider behavior.
    }
    return { tweets: annotateAndDedupe(thread, id, replies), source: 'fxtwitter' }
  } catch (error) {
    signal?.throwIfAborted()
    if (isHardNotFound(error)) throw error
  }

  const result = await fetchStatusWithFallback(handle, id, signal)
  return { ...result, tweets: annotateAndDedupe(result.tweets, id, []) }
}
