# Vendored: LocalSend core

- Source: https://github.com/localsend/localsend/tree/92e133aa43869e94d394fd04fa70c50a0f796a64/packages/core
- Commit: `92e133aa43869e94d394fd04fa70c50a0f796a64` (2026-10-30)
- License: Apache-2.0 (`LICENSE` in this folder), see also the repository `NOTICE`.
- Modifications: none. Do not edit files here; refresh with
  `cargo xtask vendor-localsend <commit>` and review the diff.

Why vendored instead of a git dependency: Cargo always checks out the
repository's submodules, and LocalSend's Flutter submodule exceeds the Windows
MAX_PATH limit, so `cargo` cannot fetch it as a git dependency on Windows.
