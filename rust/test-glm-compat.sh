#!/usr/bin/env bash
# test-glm-compat.sh — GLM Coding Plan 兼容性全面测试
# 测试 GLM Anthropic 兼容接口的各项能力
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KEY_FILE="$SCRIPT_DIR/.glm-key"
PASS=0
FAIL=0
SKIP=0

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

if [ ! -f "$KEY_FILE" ]; then
    echo "Error: .glm-key not found. Run: echo 'your-key' > $KEY_FILE"
    exit 1
fi

API_KEY="$(cat "$KEY_FILE" | tr -d '[:space:]')"
BASE="https://open.bigmodel.cn/api/anthropic/v1/messages"

api_call() {
    local body="$1"
    curl -s --max-time 60 "$BASE" \
        -H "x-api-key: $API_KEY" \
        -H "anthropic-version: 2023-06-01" \
        -H "Content-Type: application/json" \
        -d "$body" 2>/dev/null
}

check() {
    local name="$1"
    local result="$2"
    local expected="$3"

    if echo "$result" | grep -q "$expected"; then
        echo -e "  ${GREEN}PASS${NC} $name"
        ((PASS++))
    else
        echo -e "  ${RED}FAIL${NC} $name"
        echo "    Expected: $expected"
        echo "    Got: $(echo "$result" | head -c 200)"
        ((FAIL++))
    fi
}

echo "========================================"
echo "  CLAW Code × GLM Coding Plan 兼容性测试"
echo "========================================"
echo ""

# ---- Test 1: 基础对话 ----
echo "[1/8] 基础对话"
R=$(api_call '{"model":"claude-3-5-sonnet-20241022","max_tokens":20,"messages":[{"role":"user","content":"Say hello in one word"}]}')
check "简单对话" "$R" '"type":"message"'
check "有回复内容" "$R" '"content"'

# ---- Test 2: 多轮对话 ----
echo ""
echo "[2/8] 多轮对话"
R=$(api_call '{"model":"claude-3-5-sonnet-20241022","max_tokens":20,"messages":[{"role":"user","content":"My name is TestUser"},{"role":"assistant","content":"Nice to meet you, TestUser!"},{"role":"user","content":"What is my name? Reply in one word."}]}')
check "多轮上下文" "$R" "TestUser"

# ---- Test 3: System Prompt ----
echo ""
echo "[3/8] System Prompt"
R=$(api_call '{"model":"claude-3-5-sonnet-20241022","max_tokens":10,"system":"Always respond with exactly: PONG","messages":[{"role":"user","content":"Hello?"}]}')
check "System prompt 生效" "$R" "PONG"

# ---- Test 4: Tool Use (单工具) ----
echo ""
echo "[4/8] Tool Use（单工具调用）"
R=$(api_call '{
    "model":"claude-3-5-sonnet-20241022",
    "max_tokens":100,
    "tools":[{"name":"bash","description":"Run a bash command","input_schema":{"type":"object","properties":{"command":{"type":"string","description":"The command to run"}},"required":["command"]}}],
    "messages":[{"role":"user","content":"Use bash to echo hello"}]
}')
check "Tool call 返回" "$R" "tool_use"
check "Tool name 正确" "$R" "bash"

# ---- Test 5: Tool Result 反馈 ----
echo ""
echo "[5/8] Tool Result 反馈"
R=$(api_call '{
    "model":"claude-3-5-sonnet-20241022",
    "max_tokens":50,
    "tools":[{"name":"bash","description":"Run a bash command","input_schema":{"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}}],
    "messages":[
        {"role":"user","content":"Use bash to echo hello"},
        {"role":"assistant","content":[{"type":"tool_use","id":"toolu_01","name":"bash","input":{"command":"echo hello"}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"hello"}]}
    ]
}')
check "Tool result 被接受" "$R" '"type":"message"'

# ---- Test 6: Streaming ----
echo ""
echo "[6/8] Streaming（SSE）"
R=$(curl -s --max-time 15 "$BASE" \
    -H "x-api-key: $API_KEY" \
    -H "anthropic-version: 2023-06-01" \
    -H "Content-Type: application/json" \
    -d '{"model":"claude-3-5-sonnet-20241022","max_tokens":10,"stream":true,"messages":[{"role":"user","content":"Say hi"}]}' 2>/dev/null)
check "SSE 返回事件流" "$R" "event:"
check "SSE 有 content_delta" "$R" "content_block_delta"

# ---- Test 7: 长输出 ----
echo ""
echo "[7/8] 长输出（max_tokens=100）"
R=$(api_call '{"model":"claude-3-5-sonnet-20241022","max_tokens":100,"messages":[{"role":"user","content":"Count from 1 to 10, one per line"}]}')
check "长输出正常" "$R" '"stop_reason"'

# ---- Test 8: 错误处理 ----
echo ""
echo "[8/8] 错误处理"
R=$(curl -s --max-time 10 "$BASE" \
    -H "x-api-key: invalid_key_12345" \
    -H "anthropic-version: 2023-06-01" \
    -H "Content-Type: application/json" \
    -d '{"model":"claude-3-5-sonnet-20241022","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}' 2>/dev/null)
check "无效 key 返回错误" "$R" "error"

# ---- Summary ----
echo ""
echo "========================================"
TOTAL=$((PASS + FAIL))
echo -e "  Total: $TOTAL  ${GREEN}PASS: $PASS${NC}  ${RED}FAIL: $FAIL${NC}"
echo "========================================"

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
