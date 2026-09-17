#!/bin/sh
# Exercise the live-cache probe wrapper with a fake curl transport so ordinary
# tests prove explicit authorization, paired dispatch, and output sanitization
# without contacting OpenAI or requiring credentials.

set -eu

root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/mez-openai-cache-probe-test.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM
mkdir -p "$fixture_root/bin"

cat >"$fixture_root/bin/curl" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$*" >>"$FAKE_LOG"
for argument in "$@"; do
    case "$argument" in
        @*) cat "${argument#@}" >>"$FAKE_PAYLOADS" ;;
    esac
done
printf '%s\n' '{"id":"resp_fake","usage":{"input_tokens":1200,"input_tokens_details":{"cached_tokens":900,"cache_write_tokens":0}}}'
EOF
chmod +x "$fixture_root/bin/curl"

set +e
output="$(CURL_COMMAND="$fixture_root/bin/curl" PYTHON_COMMAND=python3 sh "$root/scripts/probe-openai-prompt-cache.sh" 2>&1)"
status=$?
set -e
[ "$status" -ne 0 ] || { echo "probe unexpectedly ran without opt-in" >&2; exit 1; }
printf '%s\n' "$output" | grep -F 'MEZ_OPENAI_CACHE_PROBE=1' >/dev/null

set +e
output="$(MEZ_OPENAI_CACHE_PROBE=1 OPENAI_API_KEY=secret-do-not-print MEZ_OPENAI_CACHE_PROBE_MODEL=gpt-5.6 MEZ_OPENAI_CACHE_PROBE_ENDPOINT=https://untrusted.example CURL_COMMAND="$fixture_root/bin/curl" PYTHON_COMMAND=python3 FAKE_LOG="$fixture_root/hostile.log" sh "$root/scripts/probe-openai-prompt-cache.sh" 2>&1)"
status=$?
set -e
[ "$status" -ne 0 ] || { echo "probe accepted a hostile endpoint" >&2; exit 1; }
printf '%s\n' "$output" | grep -F 'canonical direct OpenAI Responses endpoint' >/dev/null
[ ! -e "$fixture_root/hostile.log" ] || { echo "probe dispatched to a hostile endpoint" >&2; exit 1; }

output="$(MEZ_OPENAI_CACHE_PROBE=1 OPENAI_API_KEY=secret-do-not-print MEZ_OPENAI_CACHE_PROBE_MODEL=gpt-5.6 CURL_COMMAND="$fixture_root/bin/curl" PYTHON_COMMAND=python3 FAKE_LOG="$fixture_root/curl.log" FAKE_PAYLOADS="$fixture_root/payloads.json" sh "$root/scripts/probe-openai-prompt-cache.sh")"
[ "$(wc -l <"$fixture_root/curl.log" | tr -d '[:space:]')" = 2 ]
printf '%s\n' "$output" | grep -F '"backend":"direct-rest"' >/dev/null
printf '%s\n' "$output" | grep -F '"cached_tokens":900' >/dev/null
printf '%s\n' "$output" | grep -F '"append_only_from_prior":true' >/dev/null
printf '%s\n' "$output" | grep -F '"cache_ttl":"30m"' >/dev/null
printf '%s\n' "$output" | grep -E '"elapsed_millis":[0-9]+' >/dev/null
printf '%s\n' "$output" | grep -F '"key_purpose":"conformance_probe"' >/dev/null
! printf '%s\n' "$output" | grep -F 'secret-do-not-print' >/dev/null

MEZ_OPENAI_CACHE_PROBE=1 OPENAI_API_KEY=secret-do-not-print \
MEZ_OPENAI_CACHE_PROBE_MODEL=gpt-5.6 MEZ_OPENAI_CACHE_PROBE_MODE=explicit \
CURL_COMMAND="$fixture_root/bin/curl" PYTHON_COMMAND=python3 \
FAKE_LOG="$fixture_root/explicit-curl.log" FAKE_PAYLOADS="$fixture_root/explicit-payloads.json" \
sh "$root/scripts/probe-openai-prompt-cache.sh" >/dev/null
grep -F '"mode":"explicit"' "$fixture_root/explicit-payloads.json" >/dev/null
grep -F '"prompt_cache_breakpoint":{"mode":"explicit"}' "$fixture_root/explicit-payloads.json" >/dev/null
