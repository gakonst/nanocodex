use nanocodex_core::ResponseItem;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

#[derive(Serialize)]
pub(super) struct SearchRequest<'a> {
    pub(super) id: &'a str,
    pub(super) model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) input: Option<&'a [ResponseItem]>,
    pub(super) commands: &'a SearchCommands,
    pub(super) settings: SearchSettings,
    pub(super) max_output_tokens: u64,
}

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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

impl SearchCommands {
    pub(super) fn validate(&self) -> Result<(), String> {
        if !self.has_operations() {
            return Err("web.run requires at least one operation".to_owned());
        }

        let query_count = self.search_query.as_ref().map_or(0, Vec::len);
        if query_count > 4 {
            return Err(format!(
                "web.run accepts at most 4 search queries per call; got {query_count}"
            ));
        }
        if query_count > 3
            && !matches!(
                self.response_length,
                Some(SearchResponseLength::Medium | SearchResponseLength::Long)
            )
        {
            return Err(
                "web.run requires response_length medium or long when sending 4 search queries"
                    .to_owned(),
            );
        }
        Ok(())
    }

    pub(super) fn into_requests(mut self) -> Vec<Self> {
        let response_length = self.response_length;
        let mut sports = self.sports.take().unwrap_or_default().into_iter();
        self.sports = sports.next().map(|operation| vec![operation]);

        let mut requests = vec![self];
        requests.extend(sports.map(|operation| Self {
            sports: Some(vec![operation]),
            response_length,
            ..Default::default()
        }));
        requests
    }

    pub(super) fn missing_specialized_results(&self, output: &str) -> Vec<&'static str> {
        let expected = [
            (
                "finance",
                self.finance.as_ref().map_or(0, Vec::len),
                "finance",
            ),
            (
                "weather",
                self.weather.as_ref().map_or(0, Vec::len),
                "forecast",
            ),
            ("sports", self.sports.as_ref().map_or(0, Vec::len), "sports"),
            ("time", self.time.as_ref().map_or(0, Vec::len), "time"),
        ];
        expected
            .into_iter()
            .filter_map(|(name, count, reference_kind)| {
                (reference_count(output, reference_kind) < count).then_some(name)
            })
            .collect()
    }

    fn has_operations(&self) -> bool {
        [
            self.search_query.as_ref().map_or(0, Vec::len),
            self.image_query.as_ref().map_or(0, Vec::len),
            self.open.as_ref().map_or(0, Vec::len),
            self.click.as_ref().map_or(0, Vec::len),
            self.find.as_ref().map_or(0, Vec::len),
            self.finance.as_ref().map_or(0, Vec::len),
            self.weather.as_ref().map_or(0, Vec::len),
            self.sports.as_ref().map_or(0, Vec::len),
            self.time.as_ref().map_or(0, Vec::len),
        ]
        .into_iter()
        .any(|count| count > 0)
    }
}

fn reference_count(output: &str, kind: &str) -> usize {
    output
        .split("cite")
        .skip(1)
        .filter_map(|item| item.split('').next())
        .flat_map(|reference| reference.split(''))
        .filter(|reference| reference.contains(kind))
        .count()
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub(super) struct OpenOperation {
    /// Reference id or URL to open.
    pub(super) ref_id: String,
    /// Line number to position the page at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) lineno: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ClickOperation {
    /// Reference id containing the numbered link.
    pub(super) ref_id: String,
    /// Numbered link id to open.
    pub(super) id: u64,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct FindOperation {
    /// Reference id or URL to search within.
    pub(super) ref_id: String,
    /// Text pattern to find.
    pub(super) pattern: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SportsOperation {
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

impl Serialize for SportsOperation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct SportsWireOperation<'a> {
            tool: &'static str,
            r#fn: &'a SportsFunction,
            league: &'a SportsLeague,
            #[serde(skip_serializing_if = "Option::is_none")]
            team: &'a Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            opponent: &'a Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            date_from: &'a Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            date_to: &'a Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            num_games: &'a Option<u64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            locale: &'a Option<String>,
        }

        SportsWireOperation {
            tool: "sports",
            r#fn: &self.r#fn,
            league: &self.league,
            team: &self.team,
            opponent: &self.opponent,
            date_from: &self.date_from,
            date_to: &self.date_to,
            num_games: &self.num_games,
            locale: &self.locale,
        }
        .serialize(serializer)
    }
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
#[serde(deny_unknown_fields)]
pub(super) struct TimeOperation {
    /// UTC offset formatted like "+03:00".
    pub(super) utc_offset: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{SearchCommands, SearchResponseLength};

    #[test]
    fn sports_tool_is_wire_only() {
        let commands: SearchCommands = serde_json::from_value(json!({
            "sports": [{"fn": "standings", "league": "nfl"}]
        }))
        .expect("model-facing sports arguments should decode without a tool field");
        let encoded = serde_json::to_value(commands).expect("sports commands should encode");

        assert_eq!(encoded["sports"][0]["tool"], "sports");
        assert_eq!(encoded["sports"][0]["fn"], "standings");
    }

    #[test]
    fn rejects_unknown_and_removed_operations() {
        assert!(serde_json::from_value::<SearchCommands>(json!({"screenshot": []})).is_err());
        assert!(
            serde_json::from_value::<SearchCommands>(json!({
                "sports": [{"tool": "sports", "fn": "standings", "league": "nfl"}]
            }))
            .is_err()
        );
    }

    #[test]
    fn validates_query_batch_limits() {
        let commands: SearchCommands = serde_json::from_value(json!({
            "search_query": [
                {"q": "one"},
                {"q": "two"},
                {"q": "three"},
                {"q": "four"}
            ]
        }))
        .expect("search commands should decode");
        assert!(commands.validate().is_err());

        let commands = SearchCommands {
            response_length: Some(SearchResponseLength::Medium),
            ..commands
        };
        assert!(commands.validate().is_ok());
    }

    #[test]
    fn fans_out_sports_operations() {
        let commands: SearchCommands = serde_json::from_value(json!({
            "time": [{"utc_offset": "+00:00"}],
            "sports": [
                {"fn": "standings", "league": "nfl"},
                {"fn": "schedule", "league": "nba"}
            ],
            "response_length": "long"
        }))
        .expect("search commands should decode");
        let requests = commands.into_requests();

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].sports.as_ref().map(Vec::len), Some(1));
        assert_eq!(requests[0].time.as_ref().map(Vec::len), Some(1));
        assert_eq!(requests[1].sports.as_ref().map(Vec::len), Some(1));
        assert!(requests[1].time.is_none());
        assert_eq!(
            requests[1].response_length,
            Some(SearchResponseLength::Long)
        );

        let encoded = serde_json::to_value(&requests).expect("requests should encode");
        assert_eq!(encoded[0]["sports"][0]["tool"], "sports");
        assert_eq!(encoded[1]["sports"][0]["tool"], "sports");
    }

    #[test]
    fn detects_silently_omitted_specialized_results() {
        let commands: SearchCommands = serde_json::from_value(json!({
            "finance": [{"ticker": "NOT-A-TICKER", "type": "equity"}],
            "time": [{"utc_offset": "+00:00"}]
        }))
        .expect("specialized commands should decode");

        assert_eq!(
            commands.missing_specialized_results("UTC time\nciteturn0time0"),
            vec!["finance"]
        );
    }
}
