# Third-party notices

## CC Switch Codex usage parser

Parts of `src-tauri/src/codex_rebuild.rs` and the local session collectors are
adapted from CC Switch `session_usage_codex.rs`, `codex_rollout_file.rs`,
`session_usage.rs`, and `session_usage_opencode.rs`, release `v4.0.25`
(and earlier Codex work at commit `d8065cc628fcd373d00c4363d718095f19e78c9e`).

The 10 additional local-session importers under `src-tauri/agent-import/`
are adapted from [CC Switch v4.0.40](https://github.com/farion1231/cc-switch/tree/f19a9d4f4f08bda03106a7be87c77cbcdf55b019),
primarily `src-tauri/src/services/session_usage_*.rs` and minimal read-only
session-discovery helpers. TokenUsage does not embed CC Switch's proxy,
credentials, provider setup, or usage database. The directory contains the
upstream MIT license.

MIT License

Copyright (c) 2025 Jason Young

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
