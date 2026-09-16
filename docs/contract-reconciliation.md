# TypeSafe wire-contract reconciliation

This document records the offline source comparison used to prepare the Rust SDK.
It was captured on 2026-09-16. The TypeSafe evaluation service was not called; the
JSON files under `tests/fixtures/contract` are deterministic contract examples, not
recordings from a live service.

## Sources

- [HTTP OpenAPI document](https://api.typesafe.ai/openapi.json), version `0.2.0` at
  the time of review.
- [HTTP API reference](https://docs.typesafe.ai/api.md).
- [Structured-entry guide](https://docs.typesafe.ai/primitives/advanced.md).
- [Python SDK v0.6.0 question types](https://github.com/typesafe-ai/typesafe-sdk-python/blob/v0.6.0/src/typesafe_sdk/_core/question_types.py)
  and [validation](https://github.com/typesafe-ai/typesafe-sdk-python/blob/v0.6.0/src/typesafe_sdk/_core/questions.py).
- [Python SDK v0.6.0 generated wire schema](https://github.com/typesafe-ai/typesafe-sdk-python/blob/v0.6.0/src/typesafe_sdk/_schemas/models.py)
  and [retry policy](https://github.com/typesafe-ai/typesafe-sdk-python/blob/v0.6.0/src/typesafe_sdk/_core/retry.py).
- [JavaScript SDK v0.6.0 types](https://github.com/typesafe-ai/typesafe-sdk-js/blob/v0.6.0/src/types.ts),
  [question helpers](https://github.com/typesafe-ai/typesafe-sdk-js/blob/v0.6.0/src/questions.ts),
  and [retry policy](https://github.com/typesafe-ai/typesafe-sdk-js/blob/v0.6.0/src/retry.ts).

The tagged SDK sources are pinned in the links above so a future contract review can
distinguish a changed SDK from a changed interpretation.

## Comparison

| Wire element | HTTP OpenAPI | HTTP/structured docs | Python SDK | JavaScript SDK | Rust decision |
| --- | --- | --- | --- | --- | --- |
| `state` | Required string/object/array | Required string/object/array | Top-level `None` rejected | `null` allowed by `EntryType` | Keep rejecting top-level `null`; record the JS discrepancy for upstream resolution. |
| `questions` | Required map with `minProperties: 1` | Required map | Empty map rejected | Empty map rejected | Reject empty maps locally. |
| `instructions` | Optional string/object/array/`null` | `api.md` marks required/non-null; advanced guide permits `null` | Optional and nullable | Optional and nullable | Accept omitted wire fields by deserializing them as JSON `null`; preserve existing constructors and explicit values. |
| Noul criteria | Optional; sides nullable | Optional | Optional; sides nullable | Optional; sides nullable | Already aligned. |
| Choice criteria | Required map; values may be nullable | Required map | Required map; nullable descriptions | Required map; nullable descriptions | Existing nonempty-map validation remains. |
| Score criteria | Required array, `minItems: 1`; item schema excludes `null` | Required array, at least two levels | Nonempty sequence; non-null entries | At least two entries; nullable entries | Leave the existing two-level/null-entry policy unchanged until TypeSafe reconciles these conflicting sources. |
| Response `usage` | Required object with `input_tokens` and `output_tokens` | Required object | Public response tolerates missing counts | Both counts required | Keep tolerant decoding for forward compatibility. |
| `model` | Required request/response string | Required; `jev-latest` documented | Client default with override | Client default with override | Keep the current explicit default and override. |
| Retries | API docs mention 429/529 backoff | 429/529 described | Configurable statuses, jitter, connection/timeouts | Configurable statuses, jitter, connection/timeouts | Defer broader retry changes to a separate, explicitly documented PR. |

## Scope of this PR

This PR implements only behavior supported by the strongest cross-SDK/API
agreement:

1. Empty `questions` maps fail local validation instead of producing a request the
   HTTP schema marks invalid.
2. Missing `instructions` fields can be decoded from the optional wire shape. A
   caller can represent the equivalent explicit `null` with the existing constructors.
3. Offline request and response fixtures cover the documented happy path and omitted
   instructions.

It deliberately does **not** change top-level `state: null`, Score cardinality, or
Score `null` entries. Those are genuine source discrepancies, not safe assumptions to
encode while the service is unavailable.
