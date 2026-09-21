#!/usr/bin/env bash
# Portable to Bash 3.2 (including macOS /bin/bash). Requires curl and jq.
# Never enable tracing around credentials, even when invoked with bash -x.
set +x
set -euo pipefail
umask 077

usage() {
  cat <<'HELP'
Usage: scripts/inference-demo.sh [options]

List every available inference candidate, then run six diverse, independent
prompts sequentially. Each POST uses store:false, stream:false and model:auto
unless overridden. Requests are never retried automatically.

  --prompt TEXT             Add a prompt (repeatable; replaces built-ins)
  --prompts FILE             Add a JSON array of strings or {label,prompt} objects
  --models-only              Print the complete candidate catalogue, then exit
  --model MODEL              Select a model/candidate instead of auto
  --max-output-tokens N      Output budget per request, 1..4096 (default: 2048)
  --timeout SECONDS          curl request deadline, 1..3600 (default: 180)
  --output DIR               Save sanitized JSON requests/responses and summary
                            in a new directory (must not already exist)
  --no-color                Disable terminal colors (also respects NO_COLOR)
  -h, --help                Show this help

Environment:
  NANOCODEX_INFERENCE_KEY    Inference key; otherwise read silently from a TTY
  NANOCODEX_BASE_URL         API root (default:
                            https://nanocodex.gakonst.workers.dev/v1)

Examples:
  scripts/inference-demo.sh --models-only
  scripts/inference-demo.sh --prompt 'Explain a rainbow in two sentences.'
  scripts/inference-demo.sh --prompts prompts.json --output ./demo-results

The six built-ins cover explanation, code, math, writing, extraction, and
planning. Custom prompt options append in argument order. Saved artifacts
contain your prompts and model output, but no authentication headers or keys.
Candidate and family confidence are separate from Jev choice probabilities.
These scores are not task-success predictions. Missing scores stay unavailable.
HTTP TTFB is not model TTFT: this API buffers generation before responding.
HELP
}
fail() { printf 'Error: %s\n' "$1" >&2; exit 2; }
need_value() { [ "$#" -ge 2 ] && [ -n "$2" ] || fail "Missing value for $1"; }
for dependency in curl jq; do
  command -v "$dependency" >/dev/null 2>&1 || fail "Required dependency missing: $dependency"
done

model=auto
max_tokens=2048
timeout=180
output_dir=
models_only=0
no_color=0
prompts='[]'
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prompt)
      need_value "$@"
      prompts=$(printf '%s' "$prompts" | jq --arg prompt "$2" '. + [{label:("Prompt " + ((length+1)|tostring)),prompt:$prompt}]')
      shift 2 ;;
    --prompts)
      need_value "$@"
      [ -f "$2" ] && [ -r "$2" ] || fail 'Prompt file must be a readable regular file'
      incoming=$(jq -ces 'if length == 1 and (.[0]|type == "array") then .[0] else error("expected one array") end |
        if all(.[]; (type == "string" and length > 0) or
          (type == "object" and (.prompt|type == "string" and length > 0) and
            ((has("label")|not) or (.label|type == "string" and length > 0))))
        then map(if type == "string" then {prompt:.} else {label, prompt} end)
        else error("expected nonempty strings or label/prompt objects") end' "$2" 2>/dev/null) || fail 'Invalid prompts JSON: expected an array of nonempty strings or {label,prompt} objects'
      prompts=$(printf '%s\n%s\n' "$prompts" "$incoming" | jq -sc 'add | to_entries | map(.value + {label:(.value.label // ("Prompt " + ((.key+1)|tostring)))})')
      shift 2 ;;
    --model) need_value "$@"; model=$2; shift 2 ;;
    --max-output-tokens) need_value "$@"; max_tokens=$2; shift 2 ;;
    --timeout) need_value "$@"; timeout=$2; shift 2 ;;
    --output) need_value "$@"; output_dir=$2; shift 2 ;;
    --models-only) models_only=1; shift ;;
    --no-color) no_color=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail 'Unknown argument; use --help' ;;
  esac
done
case "$max_tokens" in ''|*[!0-9]*) fail '--max-output-tokens must be an integer in 1..4096' ;; esac
[ "${#max_tokens}" -le 4 ] && [ "$max_tokens" -ge 1 ] && [ "$max_tokens" -le 4096 ] || fail '--max-output-tokens must be in 1..4096'
# Normalize leading zeroes without Bash octal arithmetic.
max_tokens=$(printf '%s' "$max_tokens" | jq -R 'tonumber')
case "$timeout" in ''|*[!0-9]*) fail '--timeout must be an integer in 1..3600' ;; esac
[ "${#timeout}" -le 4 ] && [ "$timeout" -ge 1 ] && [ "$timeout" -le 3600 ] || fail '--timeout must be in 1..3600'
timeout=$(printf '%s' "$timeout" | jq -R 'tonumber')
[ "${#model}" -le 256 ] || fail '--model must be at most 256 characters'
base=${NANOCODEX_BASE_URL:-https://nanocodex.gakonst.workers.dev/v1}
base=${base%/}
# Reject URL credentials, config injection, query parameters and fragments.
case "$base" in *[[:space:]]*|*\?*|*\#*|*\@*|*\\*|*\"*) fail 'Invalid NANOCODEX_BASE_URL' ;; esac
case "$base" in
  https://?*) ;;
  http://localhost:*|http://127.0.0.1:*|http://\[::1\]:*) ;;
  *) fail 'NANOCODEX_BASE_URL must use HTTPS (HTTP allowed only on loopback)' ;;
esac
if [ -n "$output_dir" ]; then
  [ ! -e "$output_dir" ] && [ ! -L "$output_dir" ] || fail '--output must name a new directory'
fi

if [ "$prompts" = '[]' ]; then
  prompts='[
    {"label":"Explain","prompt":"Explain why the sky is blue to a curious ten-year-old in three sentences."},
    {"label":"Code","prompt":"Write a small Python function that merges two sorted integer lists in linear time. Include one example and explain its complexity."},
    {"label":"Math","prompt":"A bag has 4 red and 6 blue balls. Two balls are drawn without replacement. What is the probability that they have different colors? Show the calculation briefly."},
    {"label":"Write","prompt":"Write a vivid, hopeful story of at most 90 words about a lighthouse keeper who receives a letter from the future."},
    {"label":"Extract","prompt":"Return only JSON with person, date, time, and location from: Maya will meet the design team on 2026-10-14 at 09:30 in Room Cedar."},
    {"label":"Plan","prompt":"Create a practical five-step plan for a small volunteer group to organize a neighborhood book swap in two weeks with a $100 budget."}
  ]'
fi

# Capture then unexport the credential so child processes do not inherit it.
key=${NANOCODEX_INFERENCE_KEY:-}
unset NANOCODEX_INFERENCE_KEY
cleanup() { unset key; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
if [ -z "$key" ]; then
  [ -t 0 ] || fail 'Set NANOCODEX_INFERENCE_KEY, or run interactively to enter it silently'
  printf 'Inference key: ' >&2
  IFS= read -r -s key || { printf '\n' >&2; fail 'Could not read inference key'; }
  printf '\n' >&2
fi
# Inference keys are URL-safe tokens. Reject config/header injection outright.
case "$key" in ''|*[!a-zA-Z0-9._-]*) fail 'Inference key contains unsupported characters' ;; esac

# Strip terminal controls, including ANSI CSI/OSC, C1 controls and bidi controls.
# Literal credential redaction happens in Bash before any output or file write.
sanitize() {
  local value
  value=$(cat)
  value=${value//"$key"/[REDACTED]}
  printf '%s' "$value" | jq -Rrs '
    gsub("\u001b\\][^\u0007\u001b]*(\u0007|\u001b\\\\)"; "") |
    gsub("\u001b\\[[0-?]*[ -/]*[@-~]"; "") |
    gsub("[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]"; "")'
}
# JSON normalization removes controls from every string, not just output_text.
sanitize_json() {
  local value
  value=$(cat)
  value=${value//"$key"/[REDACTED]}
  value=$(printf '%s' "$value" | jq '
    def clean: gsub("\u001b\\][^\u0007\u001b]*(\u0007|\u001b\\\\)"; "") |
      gsub("\u001b\\[[0-?]*[ -/]*[@-~]"; "") |
      gsub("[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]"; "");
    walk(if type == "string" then clean else . end)')
  value=${value//"$key"/[REDACTED]}
  printf '%s\n' "$value"
}
# Check the JSON sanitizer before making any network calls.
jq -n 'walk(.)' >/dev/null 2>&1 || fail 'jq 1.6 or newer is required'

bold= reset= cyan=
if [ -t 1 ] && [ "$no_color" -eq 0 ] && [ "${NO_COLOR+x}" != x ]; then
  bold=$'\033[1m'; cyan=$'\033[36m'; reset=$'\033[0m'
fi
heading() { printf '\n%s%s%s\n' "$bold$cyan" "$1" "$reset"; }
if [ -n "$output_dir" ]; then
  mkdir -m 700 -- "$output_dir" || fail 'Cannot create output directory'
fi
save_json() { [ -z "$output_dir" ] || printf '%s\n' "$2" > "$output_dir/$1.json"; }

# Only the pipe carries the Authorization header. -q ignores ~/.curlrc;
# never follow redirects, retry, log headers, or put the key on argv/disk.
request() {
  local method=$1 path=$2 payload=${3:-} raw meta block line config
  http_code=000 total=unavailable ttfb=unavailable retry_after= body= curl_status=0
  config=$(printf 'header = "Authorization: Bearer %s"\n' "$key")
  if [ "$method" = POST ]; then
    # JSON is a curl config string: escape its backslashes and double quotes.
    payload=${payload//\\/\\\\}; payload=${payload//\"/\\\"}
    config="$config"$'\n'"data = \"$payload\""
  fi
  raw=$(printf '%s\n' "$config" | curl -q --config - --silent --include \
    --connect-timeout 15 --max-time "$timeout" --request "$method" \
    --header 'Content-Type: application/json' --header 'Accept: application/json' \
    --proto '=https,http' --url "$base$path" \
    --write-out $'\n__NANOCODEX_METRICS__%{http_code}\t%{time_total}\t%{time_starttransfer}' 2>/dev/null) || curl_status=$?
  unset config
  raw=${raw//"$key"/[REDACTED]}
  if [[ "$raw" == *$'\n__NANOCODEX_METRICS__'* ]]; then
    meta=${raw##*$'\n__NANOCODEX_METRICS__'}
    IFS=$'\t' read -r http_code total ttfb <<< "$meta"
    raw=${raw%$'\n__NANOCODEX_METRICS__'*}
  fi
  # Also handles interim 100 Continue / proxy CONNECT header blocks.
  while [[ "$raw" == HTTP/* && "$raw" == *$'\r\n\r\n'* ]]; do
    block=${raw%%$'\r\n\r\n'*}
    raw=${raw#*$'\r\n\r\n'}
    while IFS= read -r line; do
      case "$line" in
        [Rr][Ee][Tt][Rr][Yy]-[Aa][Ff][Tt][Ee][Rr]:*) retry_after=${line#*:}; retry_after=${retry_after%$'\r'} ;;
      esac
    done <<< "$block"
  done
  body=$raw
  json_valid=0
  if printf '%s' "$body" | jq -e -s 'length == 1 and (.[0]|type == "object")' >/dev/null 2>&1; then
    body=$(printf '%s' "$body" | sanitize_json)
    json_valid=1
  else
    # Retain malformed payloads only as a sanitized JSON error envelope.
    body=$(printf '%s' "$body" | sanitize | jq -Rs '{error:{code:"malformed_json",body:.}}')
  fi
  retry_after=$(printf '%s' "$retry_after" | sanitize)
}
report_transport() {
  if [ "$curl_status" -ne 0 ]; then
    printf 'Transport error (curl %s). Request may have reached the server; no retry was made.\n' "$curl_status"
    [ "$curl_status" -ne 28 ] || printf 'Deadline exceeded; generation/billing outcome is unknown.\n'
  elif [ "$http_code" = 429 ]; then
    printf 'Rate limited (HTTP 429). Retry-After:%s. No automatic retry.\n' "${retry_after:- unavailable}"
  elif [[ "$http_code" != 2?? ]]; then
    printf 'HTTP error: %s. No automatic retry.\n' "$http_code"
  elif [ "$json_valid" -ne 1 ]; then
    printf 'Malformed JSON response. No automatic retry.\n'
  fi
  if [ "$json_valid" -eq 1 ]; then
    printf '%s' "$body" | jq -r 'if .error then "API error: " + ((.error | if type == "object" then (.message // .code // "unknown") else . end)|tostring) else empty end'
  fi
}

heading 'Nanocodex inference / available candidates'
request GET /models
report_transport
save_json models "$body"
if [ "$curl_status" -ne 0 ] || [[ "$http_code" != 2?? ]] || [ "$json_valid" -ne 1 ]; then exit 1; fi
printf '%s' "$body" | jq -e '.data | type == "array" and all(.[]; type == "object")' >/dev/null || fail 'Invalid model catalogue: expected data array'
printf '%-43s %-14s %-30s %s\n' 'CANDIDATE ID' 'PROVIDER' 'MODEL' 'THINKING'
while IFS=$'\t' read -r id provider canonical thinking; do
  printf '%-43s %-14s %-30s %s\n' "$id" "$provider" "$canonical" "$thinking"
done < <(printf '%s' "$body" | jq -r '.data[] | [.id // "unavailable", .provider // .owned_by // "unavailable", .model // "unavailable", .thinking // "unavailable"] | @tsv')
printf '\n%s candidates available.\n' "$(printf '%s' "$body" | jq '.data|length')"
[ "$models_only" -eq 0 ] || exit 0

count=$(printf '%s' "$prompts" | jq 'length')
printf 'Running %s independent prompts; max output %s tokens; no retries.\n' "$count" "$max_tokens"
printf 'HTTP TTFB is not model TTFT (responses are buffered).\n'
summary='[]'
failed=0
index=0
while [ "$index" -lt "$count" ]; do
  item=$(printf '%s' "$prompts" | jq -c ".[$index]")
  label=$(printf '%s' "$item" | jq -r '.label' | sanitize)
  prompt=$(printf '%s' "$item" | jq -r '.prompt')
  number=$((index+1))
  heading "[$number/$count] $label"
  printf '%s\n' "$prompt" | sanitize
  payload=$(printf '%s' "$item" | jq -c --arg model "$model" --argjson tokens "$max_tokens" '{model:$model,input:.prompt,max_output_tokens:$tokens,stream:false,store:false}')
  save_json "$(printf '%02d' "$number")-request" "$(printf '%s' "$payload" | sanitize_json)"
  request POST /responses "$payload"
  report_transport
  save_json "$(printf '%02d' "$number")-response" "$body"
  state=ok
  if [ "$curl_status" -ne 0 ]; then state="curl:$curl_status"
  elif [[ "$http_code" != 2?? ]]; then state="HTTP:$http_code"
  elif [ "$json_valid" -ne 1 ]; then state=invalid-json
  elif printf '%s' "$body" | jq -e '.error != null' >/dev/null; then state=api-error
  else
    state=$(printf '%s' "$body" | jq -r '.status // "unknown"')
    case "$state" in completed|incomplete) ;; *) state=invalid-response ;; esac
  fi
  [ "$state" = completed ] || failed=1
  printf '\nStatus: %s | HTTP %s\n' "$state" "$http_code"
  # Keep unusual but valid JSON shapes from breaking the presentation pipeline.
  display_body=$(printf '%s' "$body" | jq '
    .route = (if (.route|type) == "object" then .route else {} end) |
    .usage = (if (.usage|type) == "object" then .usage else {} end) |
    .usage.input_tokens_details = (if (.usage.input_tokens_details|type) == "object" then .usage.input_tokens_details else {} end) |
    .usage.output_tokens_details = (if (.usage.output_tokens_details|type) == "object" then .usage.output_tokens_details else {} end) |
    .route.diagnostics = (if (.route.diagnostics|type) == "object" then .route.diagnostics else null end) |
    .output = (if (.output|type) == "array" then .output else [] end)')
  printf '%s' "$display_body" | jq -r '
    def show: if . == null then "unavailable" else tostring end;
    def bar: if type == "number" and . >= 0 and . <= 1 then
      . as $v | ($v*20|floor) as $n | "[" + ("#"*$n) + ("-"*(20-$n)) + "] " + (($v*10000|round)/100|tostring) + "%"
      else "unavailable" end;
    (.route // {}) as $r |
    "Provider: \($r.backend|show) | Canonical model: \($r.model // .model|show)",
    "Provider model: \($r.provider_model|show) | Effort: \($r.thinking|show)",
    "Family: \($r.family|show) | Classifier confidence: \(if $r.diagnostics != null then $r.diagnostics.family_confidence|bar else $r.confidence|bar end)",
    "Router: \($r.router_duration_ms|show) ms",
    "Usage: input=\(.usage.input_tokens|show) output=\(.usage.output_tokens|show) cached=\(.usage.input_tokens_details.cached_tokens // .usage.cached_tokens|show) reasoning=\(.usage.output_tokens_details.reasoning_tokens // .usage.reasoning_tokens|show) total=\(.usage.total_tokens|show)"'
  printf 'E2E (curl time_total): %s s | HTTP TTFB (time_starttransfer): %s s\n' "$total" "$ttfb"
  printf '%s' "$display_body" | jq -r '
    def show: if . == null then "unavailable" else tostring end;
    def bar: if type == "number" and . >= 0 and . <= 1 then
      . as $v | ($v*20|floor) as $n | "[" + ("#"*$n) + ("-"*(20-$n)) + "] " + (($v*10000|round)/100|tostring) + "%"
      else "unavailable" end;
    def pad($width): tostring as $s | $s + (" " * ([0, $width - ($s|length)]|max));
    def distribution($title; $scores):
      $title,
      if ($scores|type) == "object" and ($scores|length) > 0 then
        ($scores|to_entries|sort_by([-.value,.key])[]| "  " + (.key|pad(22)) + "  " + (.value|bar)),
        "  Reported sum: \(($scores|[.[]]|add)*10000|round|./100)% (rounding preserved)"
      else "  unavailable (not supplied as a valid complete distribution)" end;
    .route.diagnostics as $d |
    if $d == null then
      "Candidate confidence: unavailable",
      "Jev per-choice distributions: unavailable (not returned by this server)"
    else
      "Candidate confidence (selector): \($d.candidate_confidence|bar)",
      "Jev confidence status: \($d.confidence_status|show) | Minimum: \($d.min_confidence|show) | Fallback: \($d.fallback_basis|show)",
      "Proposed candidate: \($d.proposed_candidate|show)",
      "Chosen candidate: \($d.chosen_candidate|show)",
      "These are classifier/choice scores, not task-success probabilities.",
      distribution("Jev family probabilities:"; $d.family_probabilities),
      "Jev candidate probabilities (all eligible choices; * chosen, + proposed):",
      (if ($d.eligible_candidates|type) == "array" then $d.eligible_candidates|sort_by([-($d.candidate_probabilities[.] // -1),.])[] else empty end |
        . as $id | "  " + (if $id == $d.chosen_candidate then "*" else " " end) +
        (if $id == $d.proposed_candidate then "+" else " " end) + " " + ($id|pad(46)) + "  " +
        (if ($d.candidate_probabilities|type) == "object" then $d.candidate_probabilities[$id]|bar else "unavailable" end)),
      (if ($d.candidate_probabilities|type) != "object" then "  Per-choice scores unavailable; no probabilities inferred from confidence."
       else "  Reported sum: \(($d.candidate_probabilities|[.[]]|add)*10000|round|./100)% (rounding preserved)" end)
    end'
  printf '\nOutput:\n'
  printf '%s' "$display_body" | jq -r 'if (.output_text|type) == "string" then .output_text
    else [.output[]? | select(type == "object") | .content? | select(type == "array") | .[] | select(type == "object") | select(.type == "output_text" or .type == "text") | .text | select(type == "string")] | join("\n") end | if length == 0 then "(no text output returned)" else . end'
  row=$(printf '%s' "$display_body" | jq -c --arg label "$label" --arg status "$state" --arg http "$http_code" --arg total "$total" --arg ttfb "$ttfb" '{label:$label,status:$status,http_status:$http,provider:.route.backend,model:(.route.model // .model),thinking:.route.thinking,family:.route.family,family_confidence:(if .route.diagnostics != null then .route.diagnostics.family_confidence else .route.confidence end),candidate_confidence:.route.diagnostics.candidate_confidence,router_duration_ms:.route.router_duration_ms,time_total_seconds:$total,http_ttfb_seconds:$ttfb,usage:.usage}')
  summary=$(printf '%s\n%s\n' "$summary" "$row" | jq -sc '.[0] + [.[1]]')
  index=$((index+1))
  # Stop the batch on rate limits/auth failures; never multiply a known failure.
  case "$http_code" in 401|403|429) printf '\nStopping batch after HTTP %s; remaining prompts were not sent.\n' "$http_code"; break ;; esac
done
# Include prompts not sent after a batch-stopping HTTP error.
if [ "$index" -lt "$count" ]; then
  skipped=$(printf '%s' "$prompts" | jq --argjson start "$index" '.[ $start: ] | map({label,status:"not-sent",http_status:null,provider:null,model:null,thinking:null,family_confidence:null,time_total_seconds:"n/a",http_ttfb_seconds:"n/a"})' | sanitize_json)
  summary=$(printf '%s\n%s\n' "$summary" "$skipped" | jq -s 'add')
fi
heading 'Summary'
printf '%-18s %-18s %-13s %-24s %-8s %-10s %-10s %-9s %s\n' 'PROMPT' 'STATUS' 'PROVIDER' 'MODEL' 'EFFORT' 'CAND CONF%' 'FAM CONF%' 'E2E s' 'HTTP TTFB s'
while IFS=$'\t' read -r label state provider canonical thinking candidate_confidence confidence total ttfb; do
  printf '%-18s %-18s %-13s %-24s %-8s %-10s %-10s %-9s %s\n' "$label" "$state" "$provider" "$canonical" "$thinking" "$candidate_confidence" "$confidence" "$total" "$ttfb"
done < <(printf '%s' "$summary" | jq -r '.[] | [.label,.status,.provider // "n/a",.model // "n/a",.thinking // "n/a",(if (.candidate_confidence|type) == "number" then ((.candidate_confidence*10000|round)/100|tostring) else "n/a" end),(if (.family_confidence|type) == "number" then ((.family_confidence*10000|round)/100|tostring) else "n/a" end),.time_total_seconds,.http_ttfb_seconds] | @tsv')
save_json summary "$summary"
[ -z "$output_dir" ] || printf '\nSanitized JSON artifacts saved to the requested output directory.\n'
exit "$failed"
