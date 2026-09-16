# Changelog

## 0.1.0 — Unreleased

- Portable `pr_questions` CLI example, repository question files, and GitHub Actions setup guide.
- Async reqwest and blocking ureq clients for TypeSafe's System One evaluation endpoint.
- Shared sync/async client traits and independently selectable HTTP backends.
- Lazy evaluation events expose failed attempts and final results; `evaluate` drives
  the same sequence to completion.
- Generic errors retain each backend's concrete transport error type.
- Typed Noul, Choice, and Score questions and answers with structured JSON support.
- Configurable transport, typed errors, request IDs, and bounded retries.
- Original response bytes preserve exact JSON alongside convenient `f64` fields.
