# Third-Party Notices

## xai-grok-memory Dream

Portions of `qq-maid-core/src/runtime/tools/memory/dream.rs` and
`qq-maid-core/src/runtime/tools/memory/storage/dream.rs` are adapted from the Dream
implementation in
[`xai-org/grok-build`](https://github.com/xai-org/grok-build/tree/8adf9013a0929e5c7f1d4e849492d2387837a28d/crates/codegen/xai-grok-memory),
including its threshold/checkpoint flow, input truncation behavior, prompt rules, `NO_REPLY`
handling, validation boundary, and failure/success processing semantics.

Copyright 2023-2026 SpaceXAI.

Licensed under the Apache License, Version 2.0. The adapted implementation has been modified for
qq-maid-bot's SQLite Session storage, server-owned Personal/GroupProfile targets, strict JSON
output, group-member opt-out, and transactional Memory writes. A copy of the license is included
at `LICENSES/Apache-2.0.txt`.
