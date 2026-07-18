use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize)]
pub(super) struct SearchRequest<'a> {
    pub(super) id: &'a str,
    pub(super) model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) input: Option<Vec<Value>>,
    pub(super) commands: &'a SearchCommands,
    pub(super) settings: SearchSettings,
    pub(super) max_output_tokens: u64,
}

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub(super) struct SearchCommands {
    /// Query the internet search engine for a given list of queries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) search_query: Option<Vec<SearchQuery>>,
    /// Query the image search engine for a given list of queries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) image_query: Option<Vec<SearchQuery>>,
    /// Open pages by reference id or URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) open: Option<Vec<OpenOperation>>,
    /// Open links from previously opened pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) click: Option<Vec<ClickOperation>>,
    /// Find text patterns in pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) find: Option<Vec<FindOperation>>,
    /// Take screenshots of PDF pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) screenshot: Option<Vec<ScreenshotOperation>>,
    /// Look up prices for the given stock symbols.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) finance: Option<Vec<FinanceOperation>>,
    /// Look up weather forecasts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) weather: Option<Vec<WeatherOperation>>,
    /// Look up sports schedules and standings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) sports: Option<Vec<SportsOperation>>,
    /// Get time for the given UTC offsets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) time: Option<Vec<TimeOperation>>,
    /// Set the length of the response to be returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) response_length: Option<SearchResponseLength>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct SearchQuery {
    /// Search query.
    pub(super) q: String,
    /// Whether to filter by recency, as a number of recent days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) recency: Option<u64>,
    /// Whether to filter by a specific list of domains.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) domains: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct OpenOperation {
    /// Reference id or URL to open.
    pub(super) ref_id: String,
    /// Line number to position the page at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) lineno: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct ClickOperation {
    /// Reference id containing the numbered link.
    pub(super) ref_id: String,
    /// Numbered link id to open.
    pub(super) id: u64,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct FindOperation {
    /// Reference id or URL to search within.
    pub(super) ref_id: String,
    /// Text pattern to find.
    pub(super) pattern: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct ScreenshotOperation {
    /// Reference id or URL to screenshot.
    pub(super) ref_id: String,
    /// Zero-indexed PDF page number.
    pub(super) pageno: u64,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct FinanceOperation {
    /// Ticker symbol to look up.
    pub(super) ticker: String,
    /// Asset type to look up.
    pub(super) r#type: FinanceAssetType,
    /// ISO 3166-1 alpha-3 country code, "OTC", or "" for cryptocurrency.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) market: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum FinanceAssetType {
    Equity,
    Fund,
    Crypto,
    Index,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct WeatherOperation {
    /// Location in "Country, Area, City" format.
    pub(super) location: String,
    /// Start date in YYYY-MM-DD format. Defaults to today.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) start: Option<String>,
    /// Number of days to return. Defaults to 7.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) duration: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct SportsOperation {
    /// Tool name for sports requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool: Option<SportsToolName>,
    /// Sports function to call.
    pub(super) r#fn: SportsFunction,
    /// League to look up.
    pub(super) league: SportsLeague,
    /// Team to look up, using the common 3 or 4 letter alias used in broadcasts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) team: Option<String>,
    /// Opponent to use with `team` when narrowing the lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) opponent: Option<String>,
    /// Start date in YYYY-MM-DD format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) date_from: Option<String>,
    /// End date in YYYY-MM-DD format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) date_to: Option<String>,
    /// Number of games to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) num_games: Option<u64>,
    /// Locale for the lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) locale: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum SportsToolName {
    Sports,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum SportsFunction {
    Schedule,
    Standings,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum SportsLeague {
    Nba,
    Wnba,
    Nfl,
    Nhl,
    Mlb,
    Epl,
    Ncaamb,
    Ncaawb,
    Ipl,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct TimeOperation {
    /// UTC offset formatted like "+03:00".
    pub(super) utc_offset: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(super) enum SearchResponseLength {
    Short,
    Medium,
    Long,
}

#[derive(Serialize)]
pub(super) struct SearchSettings {
    pub(super) allowed_callers: [&'static str; 1],
    pub(super) external_web_access: bool,
}

#[derive(Deserialize)]
pub(super) struct SearchResponse {
    #[serde(default, rename = "encrypted_output")]
    pub(super) _encrypted_output: Option<String>,
    pub(super) output: String,
    #[serde(default)]
    pub(super) results: Option<Vec<Value>>,
}
