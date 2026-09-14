import { ConvertError } from './errors.js'
import type { FxArticle, FxArticleBlock, FxMedia, FxMediaItem, FxTweet } from './fxtwitter.js'

const SYNDICATION_BASE = 'https://cdn.syndication.twimg.com/tweet-result'
import { providerJson } from './provider.js'

interface SyndicationUser {
  name?: string
  screen_name?: string
  profile_image_url_https?: string
  description?: string
  location?: string
  followers_count?: number
  friends_count?: number
  statuses_count?: number
  created_at?: string
  verified?: boolean
  url?: string
  entities?: { url?: { urls?: Array<{ display_url?: string; expanded_url?: string }> } }
}

interface SyndicationMedia {
  type?: string
  media_url_https?: string
  url?: string
  original_info?: { width?: number; height?: number }
  sizes?: { large?: { w?: number; h?: number } }
  video_info?: {
    duration_millis?: number
    aspect_ratio?: [number, number]
    variants?: Array<{ url?: string; content_type?: string; bitrate?: number }>
  }
}

interface SyndicationArticle {
  title?: string
  preview_text?: string
  cover_media?: { media_info?: { original_img_url?: string } }
}

interface SyndicationTweet {
  id_str?: string
  text?: string
  created_at?: string
  favorite_count?: number
  conversation_count?: number
  lang?: string
  user?: SyndicationUser
  mediaDetails?: SyndicationMedia[]
  photos?: SyndicationMedia[]
  video?: SyndicationMedia
  quoted_status_result?: { result?: { legacy?: SyndicationTweet; tweet?: SyndicationTweet } }
  quoted_tweet?: SyndicationTweet
  article?: SyndicationArticle
  entities?: { media?: SyndicationMedia[] }
}

function mapMedia(raw: SyndicationTweet): FxMedia | undefined {
  const fallbackItems = [
    ...(raw.photos ?? []),
    ...(raw.video ? [raw.video] : []),
    ...(raw.entities?.media ?? []),
  ]
  const candidates = raw.mediaDetails?.length ? raw.mediaDetails : fallbackItems
  const seenMedia = new Set<string>()
  const items = candidates.filter((item) => {
    const key = item.media_url_https ?? item.url ?? item.video_info?.variants?.[0]?.url
    if (!key || seenMedia.has(key)) return false
    seenMedia.add(key)
    return true
  })
  if (items.length === 0) return undefined

  const photos: FxMediaItem[] = []
  const videos: FxMediaItem[] = []
  const animated: FxMediaItem[] = []

  for (const m of items) {
    const type = (m.type ?? 'photo').toLowerCase()
    const photoUrl = m.media_url_https ?? m.url
    const videoVariant = m.video_info?.variants
      ?.filter((v) => v.content_type?.includes('video/mp4'))
      .sort((a, b) => (b.bitrate ?? 0) - (a.bitrate ?? 0))[0]
    const variants = m.video_info?.variants
      ?.filter((v): v is { url: string; content_type?: string; bitrate?: number } => Boolean(v.url && v.content_type?.includes('video/mp4')))
      .sort((a, b) => (b.bitrate ?? 0) - (a.bitrate ?? 0))
    const width = m.original_info?.width ?? m.sizes?.large?.w
    const height = m.original_info?.height ?? m.sizes?.large?.h

    if (type === 'photo') {
      if (photoUrl) photos.push({ type: 'photo', url: photoUrl, thumbnail_url: photoUrl })
    } else if (type === 'video') {
      videos.push({ type: 'video', url: videoVariant?.url ?? photoUrl, thumbnail_url: photoUrl, width, height, duration_ms: m.video_info?.duration_millis, bitrate: videoVariant?.bitrate, variants })
    } else if (type === 'animated_gif') {
      animated.push({ type: 'animated_gif', url: videoVariant?.url ?? photoUrl, thumbnail_url: photoUrl, width, height, duration_ms: m.video_info?.duration_millis, bitrate: videoVariant?.bitrate, variants })
    }
  }

  const media: FxMedia = {}
  if (photos.length) media.photos = photos
  if (videos.length) media.videos = videos
  if (animated.length) media.animated = animated
  return Object.keys(media).length ? media : undefined
}

function mapArticle(raw?: SyndicationArticle): FxArticle | undefined {
  if (!raw?.title && !raw?.preview_text) return undefined
  const blocks: FxArticleBlock[] = []
  if (raw.preview_text) blocks.push({ type: 'unstyled', text: raw.preview_text })
  return {
    title: raw.title,
    preview_text: raw.preview_text,
    cover_media: raw.cover_media,
    content: blocks.length ? { blocks } : undefined,
  }
}

function mapUser(user?: SyndicationUser): FxTweet['author'] {
  if (!user) return undefined
  const website = user.entities?.url?.urls?.[0]
  return {
    name: user.name,
    screen_name: user.screen_name,
    url: user.screen_name ? `https://x.com/${user.screen_name}` : user.url,
    description: user.description,
    location: user.location,
    followers: user.followers_count,
    following: user.friends_count,
    statuses: user.statuses_count,
    joined: user.created_at,
    avatar_url: user.profile_image_url_https,
    website: website ? { url: website.expanded_url, display_url: website.display_url } : undefined,
    verification: { verified: user.verified },
  }
}

function mapSyndicationTweet(raw: SyndicationTweet, handle?: string, id?: string): FxTweet {
  const screenName = raw.user?.screen_name ?? handle
  const ownId = raw.id_str ?? id
  const quoted = raw.quoted_tweet ?? raw.quoted_status_result?.result?.legacy ?? raw.quoted_status_result?.result?.tweet

  return {
    id: ownId,
    text: raw.text,
    created_at: raw.created_at,
    url: screenName && ownId ? `https://x.com/${screenName}/status/${ownId}` : undefined,
    author: mapUser(raw.user),
    likes: raw.favorite_count,
    replies: raw.conversation_count,
    lang: raw.lang,
    media: mapMedia(raw),
    article: mapArticle(raw.article),
    quote: quoted ? mapSyndicationTweet(quoted) : undefined,
  }
}

export async function fetchSyndicationStatus(handle: string, id: string, signal?: AbortSignal): Promise<FxTweet> {
  const url = `${SYNDICATION_BASE}?id=${encodeURIComponent(id)}&lang=en&token=0`
  const data = await providerJson<SyndicationTweet>(url, signal)
  if (!data?.text && !data?.article) {
    throw new ConvertError(404, 'Post not found via syndication API.', 'syndication_empty')
  }

  return mapSyndicationTweet(data, handle, id)
}
