"""Source identifiers shared by CLI, import, scheduler and SQLite queries."""

AGENT_SOURCES = ("gemini", "grokbuild", "pi", "omp", "mcode", "agy", "kimi", "copilot", "workbuddy", "dsh")
NATIVE_SOURCES = ("codex", "claude", "opencode", *AGENT_SOURCES)
SOURCES = ("codex", "claude", "trae-cn", "trae", "opencode", *AGENT_SOURCES)
