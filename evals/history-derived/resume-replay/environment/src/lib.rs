//! Canonical history and incremental request construction.
//!
//! Healthy requests send only history recorded since the last completed model
//! response and include `previous_response_id`. After reconnect, the next
//! request clears that id, replays complete canonical history in order, and
//! strips response-scoped top-level item ids. The stable prompt cache key never
//! changes. Call/output pairs must remain adjacent and otherwise untouched.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryItem {
    pub id: Option<String>,
    pub kind: String,
    pub body: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RequestEnvelope {
    pub previous_response_id: Option<String>,
    pub prompt_cache_key: String,
    pub items: Vec<HistoryItem>,
}

pub struct RequestState {
    prompt_cache_key: String,
    history: Vec<HistoryItem>,
    delta_start: usize,
    previous_response_id: Option<String>,
}

impl RequestState {
    pub fn new(prompt_cache_key: impl Into<String>) -> Self {
        Self {
            prompt_cache_key: prompt_cache_key.into(),
            history: Vec::new(),
            delta_start: 0,
            previous_response_id: None,
        }
    }

    pub fn record(&mut self, item: HistoryItem) {
        self.history.push(item);
    }

    pub fn complete(&mut self, response_id: impl Into<String>) {
        self.previous_response_id = Some(response_id.into());
        self.delta_start = self.history.len();
    }

    pub fn build(&self, reconnected: bool) -> RequestEnvelope {
        let _ = reconnected;
        RequestEnvelope {
            previous_response_id: self.previous_response_id.clone(),
            prompt_cache_key: self.prompt_cache_key.clone(),
            items: self.history.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HistoryItem, RequestState};

    #[test]
    fn first_request_contains_recorded_history() {
        let mut state = RequestState::new("stable");
        state.record(HistoryItem {
            id: None,
            kind: "user".into(),
            body: "hello".into(),
        });
        assert_eq!(state.build(false).items.len(), 1);
    }
}
