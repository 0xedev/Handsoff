from handoff.adapters import (
    ClaudeAdapter,
    CodexAdapter,
    CopilotAdapter,
    CursorAdapter,
    get_adapter,
)


def test_claude_parses_anthropic_headers():
    a = ClaudeAdapter()
    headers = {
        "anthropic-ratelimit-tokens-remaining": "120000",
        "anthropic-ratelimit-tokens-reset": "2026-04-26T10:00:00Z",
        "anthropic-ratelimit-requests-remaining": "47",
        "anthropic-ratelimit-requests-reset": "2026-04-26T10:00:00Z",
        "content-type": "application/json",
    }
    s = a.parse_headers(headers)
    assert s is not None
    assert s.provider == "anthropic"
    assert s.tokens_remaining == 120000
    assert s.requests_remaining == 47
    assert s.tokens_reset_at and s.tokens_reset_at > 1_000_000_000


def test_claude_returns_none_for_non_anthropic():
    a = ClaudeAdapter()
    assert a.parse_headers({"content-type": "application/json"}) is None


def test_codex_parses_openai_headers():
    a = CodexAdapter()
    headers = {
        "x-ratelimit-remaining-tokens": "98000",
        "x-ratelimit-remaining-requests": "499",
        "x-ratelimit-reset-tokens": "1s",
        "x-ratelimit-reset-requests": "60s",
    }
    s = a.parse_headers(headers)
    assert s is not None
    assert s.provider == "openai"
    assert s.tokens_remaining == 98000
    assert s.requests_remaining == 499
    assert s.requests_reset_at  # epoch in the future


def test_copilot_parse_returns_none():
    assert CopilotAdapter().parse_headers({"x-foo": "bar"}) is None


def test_cursor_parse_returns_none():
    assert CursorAdapter().parse_headers({"x-foo": "bar"}) is None


def test_classify_host():
    assert ClaudeAdapter().classify_host("api.anthropic.com")
    assert not ClaudeAdapter().classify_host("api.openai.com")
    assert CodexAdapter().classify_host("api.openai.com")


def test_get_adapter_by_kind():
    assert get_adapter("claude").kind == "claude"
    assert get_adapter("codex").kind == "codex"


def test_claude_detect_matches_binary():
    procs = [
        {"pid": 1, "name": "bash", "cmdline": ["/bin/bash"]},
        {"pid": 42, "name": "claude", "cmdline": ["/usr/local/bin/claude", "--help"]},
        {"pid": 43, "name": "node", "cmdline": ["node", "claude-code"]},
    ]
    matches = ClaudeAdapter().detect(procs)
    pids = {m.pid for m in matches}
    assert 42 in pids
    assert 1 not in pids


def test_copilot_detect_requires_subcommand():
    procs = [
        {"pid": 100, "name": "gh", "cmdline": ["/usr/bin/gh", "issue", "list"]},
        {"pid": 101, "name": "gh", "cmdline": ["/usr/bin/gh", "copilot", "suggest"]},
    ]
    matches = CopilotAdapter().detect(procs)
    pids = {m.pid for m in matches}
    assert 100 not in pids
    assert 101 in pids


def test_cursor_detect_skips_electron_children():
    procs = [
        {"pid": 200, "name": "Cursor", "cmdline": ["/Applications/Cursor.app/Contents/MacOS/Cursor"]},
        {
            "pid": 201,
            "name": "Cursor Helper",
            "cmdline": ["/Applications/Cursor.app/Contents/Frameworks/Helper", "--type=renderer"],
        },
    ]
    matches = CursorAdapter().detect(procs)
    pids = {m.pid for m in matches}
    assert 200 in pids
    assert 201 not in pids
