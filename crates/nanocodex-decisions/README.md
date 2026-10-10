# nanocodex-decisions

Decision models answer typed questions about text or images. They return
probabilities, choices, and rubric scores instead of generated text, which
makes them a fit for classification, routing, and triage.

This crate has three parts:

- [`DecisionModel`], a provider-neutral trait over decision APIs, with the
  shared [`DecisionRequest`] and [`Decision`] types.
- `openai` (default feature): [OpenAI's Decisions
  API](https://developers.openai.com/api/docs/guides/decisions), `POST
  /v1/decisions`.
- `tool` (default feature): `DecisionTool`, which you install in an agent's
  tool registry so Responses and Claude agents can call any decision model.

## Ask questions directly

```rust,no_run
use nanocodex_decisions::{
    ChoiceOption, DecisionModel, DecisionRequest, Outcome, Question, ScoreLevel,
    openai::OpenAiDecisions,
};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let decisions = OpenAiDecisions::from_env()?;
let request = DecisionRequest::new(
    "Export fails in Safari but works in Chrome. I was also charged twice.",
    [
        Question::choice(
            "department",
            "Which department should handle this ticket first?",
            [
                ChoiceOption::new("billing").with_description("Payments and refunds."),
                ChoiceOption::new("technical").with_description("Problems using the product."),
                ChoiceOption::new("other"),
            ],
        ),
        Question::score(
            "severity",
            "How severe is the product issue?",
            [
                ScoreLevel::new("Cosmetic"),
                ScoreLevel::new("Workaround available"),
                ScoreLevel::new("Fully blocked"),
            ],
        ),
        Question::predicate("angry", "Is the customer angry?"),
    ],
)?;

let decision = decisions.decide(&request).await?;
for answer in decision.answers() {
    match &answer.outcome {
        Outcome::Predicate { probability } => println!("{}: p={probability}", answer.name),
        Outcome::Choice { choice, confidence, .. } => {
            println!("{}: {choice} ({confidence})", answer.name);
        }
        Outcome::Score { score, .. } => println!("{}: {score}", answer.name),
        Outcome::Refusal => println!("{}: refused", answer.name),
    }
}
# Ok(())
# }
```

[`DecisionRequest::new`] rejects a request before any network call if it has
no questions, an empty or repeated question name, a choice question without 2
to 255 distinct options, or a score question without levels. Every
[`Decision`] holds exactly one answer per question, in question order. An
answer either matches its question's type or is a refusal, and a choice answer
always names one of the offered options. Providers check this when they build
the result, so a malformed response becomes
[`DecisionError::InvalidResponse`] instead of a wrong answer.

## Install the tool in an agent

```rust,no_run
use nanocodex_decisions::{DecisionTool, openai::OpenAiDecisions};
use nanocodex::Tools;

# fn build() -> Result<(), Box<dyn std::error::Error>> {
let tools = Tools::builder()
    .tool(DecisionTool::new(OpenAiDecisions::from_env()?))
    .build()?;
# Ok(())
# }
```

Pass the resulting `Tools` to `NanocodexBuilder::tools`. The agent then calls
`tools.decide({ input, questions })` from Code Mode. The arguments use the same
JSON shape as [`DecisionRequest`]:

Claude agents install the same tool through `ClaudeTools::shared_tool` and call
`decide` with those arguments:

```rust,no_run
use nanocodex::{Nanocodex, claude::{Claude, ClaudeTools}};
use nanocodex_decisions::{DecisionTool, openai::OpenAiDecisions};

# fn build(claude: Claude) -> Result<(), Box<dyn std::error::Error>> {
let decisions = OpenAiDecisions::from_env()?;
let (agent, events) = Nanocodex::builder(claude)
    .tools_factory(move |_agent| {
        ClaudeTools::new().shared_tool(DecisionTool::new(decisions.clone()))
    })
    .build()?;
# Ok(())
# }
```

The arguments look like this:

```json
{
  "input": "I was charged twice for my order.",
  "questions": [
    {
      "type": "choice",
      "name": "department",
      "instructions": "Which department should handle this complaint?",
      "choices": [{ "value": "billing" }, { "value": "technical" }, { "value": "other" }]
    }
  ]
}
```

The result looks like this:

```json
{
  "model": "gpt-6-luna",
  "answers": [
    {
      "name": "department",
      "type": "choice",
      "choice": "billing",
      "confidence": 0.93,
      "probabilities": [
        { "value": "billing", "probability": 0.95 },
        { "value": "technical", "probability": 0.03 },
        { "value": "other", "probability": 0.02 }
      ]
    }
  ],
  "usage": { "input_tokens": 42, "output_tokens": 0 }
}
```

`input` may also be an array of `{ "type": "input_text", "text" }` and
`{ "type": "input_image", "image_url", "detail"? }` parts, or a JSON object
such as a support ticket. Image URLs must be base64 `data:` URLs or public
HTTP(S) URLs. OpenAI receives an object as compact JSON text. Invalid
arguments and API errors become failed tool results, so the agent can fix its
call and retry. Use `DecisionTool::with_name` to install tools backed by
different decision models side by side.

## Add a provider

Implement [`DecisionModel`] for the provider's client. Translate the request's
input and questions into the provider's wire format, and build the result with
[`Decision::new`] so the shared answer guarantees hold. Providers that key
answers by question name rather than position should arrange them in question
order first. If the provider cannot accept part of a request, such as images
for a text-only model, return [`DecisionError::InvalidRequest`] without
calling the API.

## OpenAI client notes

`OpenAiDecisions` authenticates with a Platform API key. `from_env` reads
`OPENAI_API_KEY`. Use the builder to pick a model, point at a proxy with
`base_url`, or supply a `reqwest::Client` with your own timeouts. The client
makes one attempt per call and does not retry; HTTP failures surface as
[`DecisionError::Api`] with the status and the API's error message.

## Tests

The integration tests run against a local HTTP server that stands in for the
OpenAI API, so they need no credentials:

```sh
cargo test --locked -p nanocodex-decisions
```
