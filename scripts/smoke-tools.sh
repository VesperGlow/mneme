#!/usr/bin/env bash
# Only disposable QA containers/Pod; no production ports, volumes or credentials.
set -euo pipefail
cd "$(dirname "$0")/.."
mneme_tools_image="${MNEME_TOOLS_IMAGE:-localhost/mneme-tools:qa}"
mneme_tools_pod="mneme-tools-qa-$$"
mneme_tools_container="$mneme_tools_pod-service"
mneme_tools_token=tools-qa-only-token-01234567890123456789
cleanup() { podman pod rm -f "$mneme_tools_pod" >/dev/null 2>&1 || true; }
trap cleanup EXIT
podman pod create --name "$mneme_tools_pod" >/dev/null
podman run -d --name "$mneme_tools_container" --pod "$mneme_tools_pod" \
    --read-only --user 1000:1000 --cap-drop all --security-opt no-new-privileges \
    --security-opt "seccomp=$PWD/deploy/quadlet/mneme-tools-seccomp.json" \
    --tmpfs /tmp:rw,noexec,nosuid,size=512m --pids-limit 512 --memory 1536m --cpus 1.5 \
    -e "MNEME_TOOLS_TOKEN=$mneme_tools_token" "$mneme_tools_image" >/dev/null
for attempt in $(seq 1 30); do
    if podman exec "$mneme_tools_container" node -e "fetch('http://127.0.0.1:8091/health',{headers:{Authorization:'Bearer $mneme_tools_token'}}).then(async r=>{const b=await r.json();if(!r.ok||!b.ready||!b.sandbox)process.exit(1)}).catch(()=>process.exit(1))" >/dev/null 2>&1; then break; fi
    if [ "$attempt" = 30 ]; then podman logs "$mneme_tools_container"; exit 1; fi
    sleep 1
done
# A second container shares the existing Pod's localhost, without published tool/CDP ports.
podman run --rm --pod "$mneme_tools_pod" --read-only --user 1000:1000 --cap-drop all \
    --security-opt no-new-privileges --entrypoint node "$mneme_tools_image" --input-type=module -e '
import assert from "node:assert/strict";
const base="http://127.0.0.1:8091";
assert.equal((await fetch(base+"/health")).status,401);
const headers={Authorization:"Bearer tools-qa-only-token-01234567890123456789","Content-Type":"application/json"};
const health=await (await fetch(base+"/health",{headers})).json();assert.equal(health.ready,true);assert.equal(health.sandbox,true);
const response=await fetch(base+"/v1/execute",{method:"POST",headers,body:JSON.stringify({session_id:"11111111-1111-4111-8111-111111111111",arguments:{action:"navigate",url:"http://127.0.0.1:7474/"}})});
assert.equal(response.status,400);assert.equal((await response.json()).error,"blocked_destination");
console.log("PASS: shared Pod localhost HTTP, independent auth, Chromium sandbox, internal Neo4j SSRF rejection, no tool/CDP port publishing");'
podman run --rm --network none --read-only --user 1000:1000 --cap-drop all \
    --security-opt no-new-privileges --security-opt "seccomp=$PWD/deploy/quadlet/mneme-tools-seccomp.json" \
    --tmpfs /tmp:rw,noexec,nosuid,size=512m --pids-limit 512 --memory 1536m --cpus 1.5 \
    -v "$PWD/services/tools/test:/app/test:ro" -e MNEME_BROWSER_TEST=1 \
    --entrypoint node "$mneme_tools_image" --test --test-reporter=tap --test-timeout=60000 /app/test/security.test.mjs /app/test/browser.test.mjs
