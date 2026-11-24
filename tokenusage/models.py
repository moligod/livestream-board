from __future__ import annotations

from dataclasses import asdict, dataclass


@dataclass(slots=True)
class UsageRecord:
    source: str
    session_id: str
    timestamp: int
    project: str = "unknown"
    model: str = "unknown"
    input_tokens: int = 0
    output_tokens: int = 0
    reasoning_tokens: int = 0
    cache_read_tokens: int = 0
    cache_write_tokens: int = 0
    total_tokens: int = 0
    cost: float = 0.0
    record_id: str | None = None

    def normalized(self) -> "UsageRecord":
        if not self.total_tokens:
            self.total_tokens = self.input_tokens + self.output_tokens
        return self

    def to_dict(self) -> dict:
        return asdict(self)
