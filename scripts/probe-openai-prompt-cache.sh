#!/bin/sh
# Run an explicitly authorized, content-safe paired OpenAI Responses cache
# observation. The script never reads Mezzanine configuration or credential
# stores: callers supply an API key through the environment and receive only
# sanitized usage and request-shape observations on stdout.

set -eu

fail() {
    printf '%s\n' "OpenAI prompt-cache probe: $*" >&2
    exit 1
}

[ "${MEZ_OPENAI_CACHE_PROBE:-}" = "1" ] ||
    fail "set MEZ_OPENAI_CACHE_PROBE=1 to authorize live provider requests"
[ -n "${OPENAI_API_KEY:-}" ] || fail "requires OPENAI_API_KEY"
[ -n "${MEZ_OPENAI_CACHE_PROBE_MODEL:-}" ] ||
    fail "requires MEZ_OPENAI_CACHE_PROBE_MODEL"

curl_command="${CURL_COMMAND:-curl}"
python_command="${PYTHON_COMMAND:-python3}"
command -v "$curl_command" >/dev/null 2>&1 || fail "requires $curl_command"
command -v "$python_command" >/dev/null 2>&1 || fail "requires $python_command"

endpoint="https://api.openai.com/v1/responses"
if [ -n "${MEZ_OPENAI_CACHE_PROBE_ENDPOINT:-}" ] &&
    [ "$MEZ_OPENAI_CACHE_PROBE_ENDPOINT" != "$endpoint" ]; then
    fail "MEZ_OPENAI_CACHE_PROBE_ENDPOINT must be the canonical direct OpenAI Responses endpoint"
fi
mode="${MEZ_OPENAI_CACHE_PROBE_MODE:-implicit}"
timeout_seconds="${MEZ_OPENAI_CACHE_PROBE_TIMEOUT_SECONDS:-60}"
case "$mode" in
    implicit|explicit) ;;
    *) fail "MEZ_OPENAI_CACHE_PROBE_MODE must be implicit or explicit" ;;
esac
if [ "$mode" = explicit ]; then
    case "$MEZ_OPENAI_CACHE_PROBE_MODEL" in
        gpt-5.6*|gpt-6*) ;;
        *) fail "explicit mode requires a GPT-5.6-or-newer probe model" ;;
    esac
fi
case "$timeout_seconds" in
    ''|*[!0-9]*) fail "MEZ_OPENAI_CACHE_PROBE_TIMEOUT_SECONDS must be a positive integer" ;;
esac
[ "$timeout_seconds" -gt 0 ] || fail "MEZ_OPENAI_CACHE_PROBE_TIMEOUT_SECONDS must be positive"

work_root="$(mktemp -d "${TMPDIR:-/tmp}/mez-openai-cache-probe.XXXXXX")"
trap 'rm -rf "$work_root"' EXIT HUP INT TERM

build_payload() {
    ordinal=$1
    "$python_command" - "$MEZ_OPENAI_CACHE_PROBE_MODEL" "$mode" "$ordinal" <<'PY'
import json
import sys

model, mode, ordinal = sys.argv[1:]
ordinal = int(ordinal)
is_gpt56_or_newer = model.lower().startswith(("gpt-5.6", "gpt-6"))
prefix = "Mezzanine OpenAI cache conformance probe. This synthetic stable prefix contains no user, workspace, credential, or action data. " * 96
body = {
    "model": model,
    "store": False,
    "parallel_tool_calls": False,
    "prompt_cache_key": "mez-openai-cache-conformance-v1",
    "instructions": "Return exactly the word ok.",
    "input": [{"role": "developer", "content": [{"type": "input_text", "text": prefix}]}] + [
        {"role": "user", "content": [{"type": "input_text", "text": f"probe suffix {index}"}]}
        for index in range(1, ordinal + 1)
    ],
}
if is_gpt56_or_newer:
    body["prompt_cache_options"] = {"ttl": "30m"}
    if mode == "explicit":
        body["prompt_cache_options"]["mode"] = "explicit"
        body["input"][0]["content"][0]["prompt_cache_breakpoint"] = {"mode": "explicit"}
print(json.dumps(body, separators=(",", ":")))
PY
}

send_probe() {
    ordinal=$1
    payload_file="$work_root/request-$ordinal.json"
    response_file="$work_root/response-$ordinal.json"
    build_payload "$ordinal" >"$payload_file"
    started_at="$($python_command -c 'import time; print(time.monotonic())')"
    "$curl_command" --fail-with-body --silent --show-error --max-time "$timeout_seconds" \
        -X POST "$endpoint" \
        -H "Authorization: Bearer $OPENAI_API_KEY" \
        -H 'Content-Type: application/json' \
        --data-binary "@$payload_file" >"$response_file"
    finished_at="$($python_command -c 'import time; print(time.monotonic())')"
    "$python_command" - "$payload_file" "$response_file" "$ordinal" "$MEZ_OPENAI_CACHE_PROBE_MODEL" "$mode" "$started_at" "$finished_at" <<'PY'
import hashlib
import json
import sys

payload_path, response_path, ordinal, model, mode, started_at, finished_at = sys.argv[1:]
with open(response_path, encoding="utf-8") as source:
    body = json.load(source)
with open(payload_path, "rb") as source:
    request_shape_sha256 = hashlib.sha256(source.read()).hexdigest()
usage = body.get("usage") or {}
details = usage.get("input_tokens_details") or {}
output_details = usage.get("output_tokens_details") or {}
if model.lower().startswith("gpt-5.6") or model.lower().startswith("gpt-6"):
    generation = "gpt56_or_newer"
elif model.lower().startswith("gpt-5.5"):
    generation = "gpt55"
else:
    generation = "pre_gpt56_or_unknown"
observation = {
    "backend": "direct-rest",
    "append_only_from_prior": int(ordinal) > 1,
    "cache_generation": generation,
    "model": model,
    "cache_mode": mode,
    "cache_ttl": "30m" if generation == "gpt56_or_newer" else None,
    "elapsed_millis": round((float(finished_at) - float(started_at)) * 1000),
    "key_partition_sha256": hashlib.sha256(b"mez-openai-cache-conformance-v1").hexdigest(),
    "key_purpose": "conformance_probe",
    "request": int(ordinal),
    "request_shape_sha256": request_shape_sha256,
    "input_tokens": usage.get("input_tokens"),
    "cached_tokens": details.get("cached_tokens"),
    "cache_write_tokens": details.get("cache_write_tokens", output_details.get("cache_write_tokens")),
    "response_id_present": isinstance(body.get("id"), str),
}
print(json.dumps(observation, sort_keys=True, separators=(",", ":")))
PY
}

send_probe 1
send_probe 2
