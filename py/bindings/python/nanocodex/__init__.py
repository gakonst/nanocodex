"""Embedded Python bindings for the Nanocodex agents SDK."""

from typing import TypedDict

from ._native import (
    AgentEvent,
    AgentEvents,
    Nanocodex,
    SessionSnapshot,
    Turn,
    TurnResult,
    __version__,
)


class EstimatedCost(TypedDict):
    usd: str
    input_usd: str
    cached_input_usd: str
    cache_write_input_usd: str
    output_usd: str
    service_tier: str


class Usage(TypedDict):
    input_tokens: int
    cached_input_tokens: int
    cache_write_input_tokens: int
    output_tokens: int
    reasoning_output_tokens: int
    total_tokens: int
    estimated_cost: EstimatedCost | None
    cost_status: str


__all__ = [
    "AgentEvent",
    "AgentEvents",
    "EstimatedCost",
    "Nanocodex",
    "SessionSnapshot",
    "Turn",
    "TurnResult",
    "Usage",
    "__version__",
]
