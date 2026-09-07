import { ConvertError } from './errors.js'

const FX_BASE = 'https://api.fxtwitter.com'
import { providerJson } from './provider.js'

export interface FxAuthor {
  id?: string
  name?: string
  screen_name?: string
  url?: string
  description?: string
  location?: string
  followers?: number
  following?: number
  likes?: number
  media_count?: number
  statuses?: number
  joined?: string
  avatar_url?: string
  banner_url?: string
  protected?: boolean
  website?: { url?: string; display_url?: string }
  verification?: { verified?: boolean; type?: string }
}

export interface FxMediaItem {
  type?: string
  url?: string
  thumbnail_url?: string
  width?: number
  height?: number
  duration?: number
  /** Normalized duration. FxTwitter sends seconds; syndication sends milliseconds. */
  duration_ms?: number
  format?: string
  bitrate?: number
  alt?: string
  altText?: string
  variants?: Array<{
    url: string
    content_type?: string
    bitrate?: number
  }>
  formats?: Array<{
    url: string
    container?: string
    codec?: string
    bitrate?: number
  }>
}

export interface FxMosaic {
  type?: string
  photos?: FxMediaItem[]
  formats?: { jpeg?: string; webp?: string }
}

export interface FxMedia {
  photos?: FxMediaItem[]
  videos?: FxMediaItem[]
  animated?: FxMediaItem[]
  mosaic?: FxMosaic
  all?: FxMediaItem[]
}

export interface FxPollChoice {
  label?: string
  count?: number
  percentage?: number
}

export interface FxPoll {
  choices?: FxPollChoice[]
  total_votes?: number
  time_left_en?: string
  ends_at?: string
}

export interface FxArticleBlock {
  type?: string
  text?: string
  inlineStyleRanges?: Array<{ offset: number; length: number; style: string }>
  data?: { urls?: Array<{ fromIndex: number; toIndex: number; text: string }> }
}

export interface FxArticle {
  title?: string
  preview_text?: string
  content?: { blocks?: FxArticleBlock[] }
  cover_media?: {
    media_info?: { original_img_url?: string }
  }
}

/** FxTwitter may return a single parent object or legacy string[] handles. */
export type FxReplyingTo =
  | string[]
  | {
      screen_name?: string
      status?: string
      url?: string
      profile_url?: string
    }
  | null

export interface FxTweet {
  url?: string
  id?: string
  text?: string
  created_at?: string
  created_timestamp?: number
  author?: FxAuthor
  replies?: number
  retweets?: number
  reposts?: number
  likes?: number
  views?: number | null
  bookmarks?: number
  quotes?: number
  lang?: string
  source?: string
  replying_to?: FxReplyingTo
  replying_to_status?: string[] | null
  possibly_sensitive?: boolean
  media?: FxMedia
  quote?: FxTweet
  reposted_by?: FxAuthor | null
  article?: FxArticle
  poll?: FxPoll
  community_note?: unknown
  /** How this post relates to the status requested by the caller. */
  context?: 'parent' | 'post' | 'thread' | 'reply'
}

interface FxApiResponse {
  code?: number
  message?: string
  tweet?: FxTweet
  status?: FxTweet
  thread?: FxTweet[]
}

interface FxConversationResponse {
  results?: FxTweet[]
  tweets?: FxTweet[]
  conversation?: FxTweet[]
  replies?: FxTweet[] | null
}

export interface FxCursor {
  top?: string
  bottom?: string
}

export interface FxListResponse<T> {
  results: T[]
  cursor?: FxCursor
}

function normalizeMediaItem(item: FxMediaItem): FxMediaItem {
  const formatVariants = item.formats?.filter((format) => format.url).map((format) => ({
    url: format.url,
    content_type: format.container?.includes('/')
      ? format.container
      : format.container === 'mp4'
      ? 'video/mp4'
      : format.container === 'webm'
        ? 'video/webm'
        : format.container === 'm3u8'
          ? 'application/vnd.apple.mpegurl'
          : undefined,
    bitrate: format.bitrate,
  }))
  return {
    ...item,
    alt: item.alt ?? item.altText,
    duration_ms: item.duration_ms ?? (item.duration != null ? item.duration * 1000 : undefined),
    variants: item.variants ?? formatVariants,
  }
}

function normalizeMedia(media?: FxMedia): FxMedia | undefined {
  if (!media || Object.keys(media).length === 0) return undefined
  const map = (items?: FxMediaItem[]) => items?.map(normalizeMediaItem)
  return {
    ...media,
    photos: map(media.photos),
    videos: map(media.videos),
    animated: map(media.animated),
    all: map(media.all),
    mosaic: media.mosaic ? { ...media.mosaic, photos: map(media.mosaic.photos) } : undefined,
  }
}

function normalizeTweet(raw: FxTweet): FxTweet {
  return {
    ...raw,
    retweets: raw.retweets ?? raw.reposts,
    media: normalizeMedia(raw.media),
    quote: raw.quote ? normalizeTweet(raw.quote) : undefined,
  }
}

function pickTweet(data: FxApiResponse): FxTweet | undefined {
  const raw = data.status ?? data.tweet
  return raw ? normalizeTweet(raw) : undefined
}

async function fxFetchJson<T>(path: string, signal?: AbortSignal): Promise<T> {
  const data = await providerJson<FxApiResponse>(`${FX_BASE}/${path}`, signal)

  if (data.code === 429 || data.code === 503) {
    throw new ConvertError(data.code, 'X data provider is temporarily unavailable.',
      data.code === 429 ? 'rate_limited' : 'provider_unavailable', 30)
  }

  if (data.code === 404 || data.message === 'NOT_FOUND') {
    throw new ConvertError(404, 'Post not found or unavailable.', 'not_found')
  }

  if (data.message === 'PRIVATE_TWEET') {
    throw new ConvertError(404, 'Post is private and cannot be fetched.', 'private_tweet')
  }

  if ((data.code && data.code >= 400)) {
    throw new ConvertError(
      502,
      `FxTwitter API error: ${data.message ?? data.code}.`,
      'fxtwitter_error',
    )
  }

  return data as T
}

async function fxFetch(path: string, signal?: AbortSignal): Promise<FxApiResponse> {
  return fxFetchJson<FxApiResponse>(path, signal)
}

function encodeQuery(params: Record<string, string | number | undefined>): string {
  const query = new URLSearchParams()
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== '') query.set(key, String(value))
  }
  return query.toString()
}

export async function fetchFxProfile(handle: string, signal?: AbortSignal): Promise<FxAuthor> {
  const data = await fxFetchJson<{ user?: FxAuthor }>(`2/profile/${encodeURIComponent(handle)}`, signal)
  if (!data.user) throw new ConvertError(404, 'Profile not found.', 'not_found')
  return data.user
}

export async function fetchFxProfileStatuses(
  handle: string,
  cursor?: string,
  count = 20,
  signal?: AbortSignal,
): Promise<FxListResponse<FxTweet>> {
  const query = encodeQuery({ cursor, count, with_replies: 'false' })
  const data = await fxFetchJson<Partial<FxListResponse<FxTweet>>>(
    `2/profile/${encodeURIComponent(handle)}/statuses?${query}`, signal,
  )
  return { results: (data.results ?? []).map(normalizeTweet), cursor: data.cursor }
}

export async function searchFxStatuses(
  queryText: string,
  feed: string,
  cursor?: string,
  count = 20,
  signal?: AbortSignal,
): Promise<FxListResponse<FxTweet>> {
  const query = encodeQuery({ q: queryText, feed, cursor, count })
  let data: Partial<FxListResponse<FxTweet>>
  try {
    data = await fxFetchJson<Partial<FxListResponse<FxTweet>>>(`2/search?${query}`, signal)
  } catch (error) {
    // FxTwitter answers `{code:404, results:[]}` when X's SearchTimeline gives it no
    // timeline at all (upstream account/session failure, see FxEmbed#2303). That is
    // an outage, not a missing post, so surface it as retryable.
    if (error instanceof ConvertError && error.code === 'not_found') {
      throw new ConvertError(503, 'X search is temporarily unavailable upstream. Retry shortly.', 'search_unavailable')
    }
    throw error
  }
  return { results: (data.results ?? []).map(normalizeTweet), cursor: data.cursor }
}

export async function fetchFxConnections(
  handle: string,
  relation: 'followers' | 'following',
  cursor?: string,
  count = 20,
  signal?: AbortSignal,
): Promise<FxListResponse<FxAuthor>> {
  const query = encodeQuery({ cursor, count })
  const data = await fxFetchJson<Partial<FxListResponse<FxAuthor>>>(
    `2/profile/${encodeURIComponent(handle)}/${relation}?${query}`, signal,
  )
  return { results: data.results ?? [], cursor: data.cursor }
}

export async function fetchFxStatus(id: string, signal?: AbortSignal): Promise<FxTweet> {
  const data = await fxFetch(`2/status/${id}`, signal)
  const tweet = pickTweet(data)
  if (!tweet) {
    throw new ConvertError(404, 'Post not found.', 'not_found')
  }
  return tweet
}

export function getParentStatusId(tweet: FxTweet): string | undefined {
  const replyingTo = tweet.replying_to
  if (replyingTo && !Array.isArray(replyingTo)) {
    const status = replyingTo.status
    if (status) return String(status)
  }

  const fromStatus = tweet.replying_to_status?.[0]
  if (fromStatus) return String(fromStatus)

  return undefined
}

/** Walk parent replies from root through the given status id (inclusive). */
export async function fetchFxConversationChain(
  id: string,
  signal?: AbortSignal,
): Promise<FxTweet[]> {
  const walked: FxTweet[] = []
  const seen = new Set<string>()
  let currentId: string | undefined = id

  while (currentId && walked.length < 100) {
    if (seen.has(currentId)) break
    seen.add(currentId)

    try {
      const tweet = await fetchFxStatus(currentId, signal)
      walked.push(tweet)
      currentId = getParentStatusId(tweet)
    } catch (error) {
      signal?.throwIfAborted()
      if (error instanceof ConvertError && ['private_tweet', 'rate_limited'].includes(error.code ?? '')) throw error
      break
    }
  }

  walked.reverse()
  return walked
}

export async function fetchFxThread(id: string, signal?: AbortSignal): Promise<FxTweet[]> {
  const data = await fxFetch(`2/thread/${id}`, signal)
  if (data.thread?.length) {
    return data.thread.map(normalizeTweet)
  }
  return [await fetchFxStatus(id, signal)]
}

export type FxReplyRanking = 'likes' | 'recency'

/** Fetch ranked replies from FxTwitter's v2 conversation endpoint. */
export async function fetchFxConversationReplies(
  id: string,
  rankingMode: FxReplyRanking = 'likes',
  limit = 10,
  signal?: AbortSignal,
): Promise<FxTweet[]> {
  const query = encodeQuery({ ranking_mode: rankingMode })
  const data = await fxFetchJson<FxConversationResponse>(
    `2/conversation/${encodeURIComponent(id)}?${query}`, signal,
  )
  const results = (data.replies ?? data.results ?? data.tweets ?? data.conversation ?? [])
    .map(normalizeTweet)
    .filter((tweet) => getParentStatusId(tweet) === id)
  const timestamp = (tweet: FxTweet) => tweet.created_timestamp != null
    ? tweet.created_timestamp
    : Date.parse(tweet.created_at ?? '') || 0
  results.sort((a, b) => {
    const primary = rankingMode === 'likes'
      ? (b.likes ?? 0) - (a.likes ?? 0)
      : timestamp(b) - timestamp(a)
    return primary || timestamp(b) - timestamp(a) || String(a.id ?? '').localeCompare(String(b.id ?? ''))
  })
  return results.slice(0, limit)
}

/**
 * Full thread: FxTwitter thread endpoint (author threads + reply chains), with a
 * parent-walk fallback when the endpoint returns only the requested status.
 */
export async function fetchFxFullThread(id: string, signal?: AbortSignal): Promise<FxTweet[]> {
  const fromThread = await fetchFxThread(id, signal)
  if (fromThread.length > 1) {
    return fromThread
  }

  const tweet = fromThread[0] ?? (await fetchFxStatus(id, signal))
  if (!getParentStatusId(tweet)) {
    return [tweet]
  }

  const chain = await fetchFxConversationChain(id, signal)
  return chain.length > 0 ? chain : [tweet]
}
