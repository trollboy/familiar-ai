# PRD-008: Local Model Manager

## Overview

Lifecycle management for optional local LLM. Trait-based interface with pluggable backends.

## Depends On

- PRD-001: Config + LLM toggle
- PRD-004: Systray toggle

## Scope

- LLM interface trait (summarize, classify, embed)
- Lifecycle: load, unload, health check
- Backend implementations: stub, mistral.rs, Ollama HTTP, external endpoint
- Memory management (unload from RAM via systray)
- Config: model path, backend selection, resource limits
