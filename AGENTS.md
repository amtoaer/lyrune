## Code style

- Prefer minimal, direct changes.
- Make the smallest diff that correctly solves the task.
- Do not refactor unrelated code.
- Do not introduce new abstractions unless the existing code already clearly needs them.
- Do not add defensive fallbacks, compatibility layers, logging, comments, or config unless explicitly requested.
- Prefer existing project patterns over creating new helpers.
- Preserve naming, file layout, formatting, and architecture unless the task requires changing them.
- When modifying existing code, edit in place instead of rewriting the whole file.
- If the change can be done in under ~20 lines, do not split it into extra functions/classes unless readability clearly improves.
- Before implementing, briefly state the intended minimal patch.
- After implementing, summarize only what changed and what was verified.
- Keep added tests few and focused on critical behavior. Avoid exhaustive cases and tests that merely repeat the implementation.
- Prefer concise test names matching existing patterns, such as `test_generate_nfo` and `test_status_update`.

## Commit messages

- Use the format `<type>: 中文描述`.
- Use a conventional type such as `feat`, `chore`, `fix`, or `refactor`.

## Branches

- Choose a branch name that matches the change's function.
- Use prefixes such as `feat/<topic>`, `fix/<topic>`, `refactor/<topic>`, or `chore/<topic>`.
- Before pushing, verify that the current branch matches the work instead of using a generic feature branch.

## Proportional engineering

- Make the smallest coherent change that fully solves the request.
- Do not add abstractions, helpers, wrappers, interfaces, configuration, dependencies, or refactors unless they remove demonstrated duplication or are required for correctness.
- Keep a small piece of logic inline when extracting it would only save a few lines or create a single-use function.
- Do not create a function solely to make a module look more modular.
- Prefer existing functions and local control flow when the behavior is simple and used once.
- Before extracting a function, identify at least two concrete call sites or a meaningful ownership, lifecycle, or testing boundary. Two similar call sites justify considering extraction, not extraction itself.
- Before sharing logic, assess how much behavior is genuinely common at the proposed boundary and how much must remain specific to each caller. Weigh the duplication removed against added parameters, branches, conversions, and indirection; similar syntax alone is not evidence of a shared responsibility.
- When caller-specific behavior dominates and the common part is mostly glue, prefer separate direct flows that reuse existing lower-level operations. Prefer consolidating repeated control flow in place when it avoids the need for a new abstraction.
- Treat the shared proportion as a judgment aid, not a fixed percentage or line-count threshold. A small common part can still merit extraction when it enforces an important invariant or a clear lifecycle boundary; explain that concrete benefit rather than relying on code similarity alone.

## Proportional testing

- Add or update tests only when the change introduces meaningful behavior or regression risk.
- Do not add tests for trivial getters/setters, direct pass-through code, simple glue, obvious constructors, formatting-only changes, or behavior already covered by a higher-level test.
- Prefer a small number of behavior-focused tests over one test per branch, helper, or tiny function.
- Test public behavior and important invariants, not private helper structure or implementation details.
- Prefer existing tests and test patterns; do not create a new fixture, mock, trait, or test utility for a single low-value case.
- Run the narrowest relevant test command first. Run the full suite only when the change affects shared infrastructure, public APIs, or broad behavior.
- If no test is added, briefly state why the existing coverage and change risk make a new test unnecessary.

## Reference project style: `amtoaer/bili-sync`

The following summary is based on reading a shallow clone of `https://github.com/amtoaer/bili-sync`, `main` at commit `c900777e78796653fdfb00421879b6a802d1eedd`. Repeated patterns are evidence of likely preferences, not blanket rules or approval of a Lyrune architecture. The guidance below combines those observations with this repository's explicit rules; preservation requirements and GPUI-specific constraints are Lyrune requirements, not claims about bili-sync. When an inference would materially affect a design decision outside the developer's instructions for the task, explain the evidence and ask the developer first.

Representative evidence (paths relative to the reference repository at that commit):

- `crates/bili_sync/src/config/mod.rs` and `task/mod.rs`: private modules and selective re-exports; `bilibili/mod.rs` also owns shared types and implementation, so a facade-only entry point is not universal.
- `crates/bili_sync/src/workflow.rs` and `adapter/mod.rs`: direct domain orchestration, stream concurrency, and a trait with several concrete video-source implementations and enum dispatch.
- `crates/bili_sync/src/main.rs`, `downloader.rs`, and `config/versioned_config.rs`: task lifetime management, async I/O, error context, and configuration snapshots/persistence ordering.
- `crates/bili_sync/src/bilibili/credential.rs` and `utils/status.rs`: protocol-local logic, enums, explicit invariants, colocated tests, and ignored manual login tests.
- `rustfmt.toml`: explicit import grouping and line width. Preserve Lyrune's own formatter configuration unless a separate change is approved.

Do not infer a ban on traits, generics, small helpers, or names ending in `Manager`/`Context`: the reference has `VideoSource`, `Status<N, C>`, `DownloadTaskManager`, and `DownloadContext`. It also contains empty credential strings, wildcard imports, Chinese and English diagnostics, and both explanatory and step-by-step comments. These choices need context; they do not establish a universal preference for or against each construct. Generated entity/protobuf code and borrowed implementations are not primary evidence of personal style.

### Structure and ownership

- Organize code by domain responsibility rather than by generic technical layers. The reference project uses modules such as `bilibili`, `config`, `adapter`, `api`, `task`, `notifier`, `utils`, and `workflow`, with submodules named after concrete concepts.
- Use private implementation modules and selective `pub use` to expose a useful domain surface where appropriate. Keep cohesive shared types or logic in the entry point when that makes ownership clearer; do not require every `mod.rs` to be facade-only.
- Prefer a small number of meaningful domain modules over many tiny files. A file may contain a cohesive group of related operations; file length alone is not a sufficient reason to split it.
- Use concrete types and direct control flow. Introduce a trait when there is real polymorphism or a meaningful boundary, not merely to hide a few lines or create a generic service/repository/controller layer.
- When a closed set of variants needs common behavior, prefer an explicit enum and a focused dispatch mechanism. The reference project uses enums and `enum_dispatch` where the variants are known, rather than an open-ended plugin hierarchy.
- Keep persistence, protocol/client code, orchestration, and presentation concerns distinguishable. A function may orchestrate several domain operations, but the ownership of each operation should remain apparent.
- Preserve the existing project vocabulary. Names should describe the domain object or operation directly (`VideoSource`, `Credential`, `VersionedConfig`, `process_video_source`) instead of introducing broad names such as `Manager`, `Service`, or `Context` without a precise responsibility.

### Rust syntax and formatting

- Use Rust 2024 conventions and the repository's formatter settings. The reference project uses `rustfmt.toml` with `group_imports = "StdExternalCrate"`, `imports_granularity = "Module"`, and `max_width = 120`.
- Group imports as standard library, external crates, then local crate imports. Prefer importing the exact items used and use module-level re-exports to keep call sites readable.
- Prefer `let-else`, early returns, `match`, and `if let`/let chains when they make success and failure paths visible. Avoid deeply nested `if` blocks and boolean-heavy control flow.
- Use iterators and combinators when they express the transformation clearly, but do not turn simple imperative state changes into opaque iterator chains. `for`/`while` loops are appropriate for stateful workflows and streaming operations.
- Use `Option` and `Result` where they express the domain clearly, but do not infer a blanket ban on sentinel values from the reference. Changing existing empty-string semantics requires confirmation and must preserve serialization, failure, and retry behavior.
- Use `derive` for routine serialization, validation, and small value-type behavior. Keep manual implementations for actual invariants or conversions rather than adding boilerplate abstractions.

### Errors, invariants, and fallibility

- Use `anyhow::Result` for application-level fallible operations, and add context at I/O, database, network, parsing, and task boundaries with `context`/`with_context`.
- Use `bail!` and `ensure!` for local precondition and protocol validation failures. Make messages identify the operation and the relevant value when that information helps the caller diagnose the failure.
- Use a specific error enum when callers need to distinguish variants programmatically; otherwise preserve the error chain rather than flattening it into a string.
- Treat `unwrap`/`expect` as a statement that a condition is impossible or required for startup/static initialization. Use `Result` and explicit handling for runtime failures or user-controlled data.
- Preserve domain invariants with enums, typed values, assertions, or validation methods where they make the invariant easier to see. Do not add defensive fallback behavior that silently changes the existing result.
- Comments are valuable when they explain why an apparently unusual choice is required: an external API quirk, an async/blocking boundary, database ordering, ownership limitation, compatibility rule, or safety invariant. Do not add comments that merely paraphrase the next line.
- Public or non-obvious APIs may have concise `///` documentation. Documentation should explain the contract, lifecycle, or invariant, not repeat the function name.

### Async, concurrency, and external I/O

- Keep blocking work off async executors with `spawn_blocking` or an equivalent existing boundary. Do not make a synchronous keychain, filesystem, process, or database operation look asynchronous merely by wrapping it in an async function.
- Use Tokio and existing concurrency primitives directly: streams, `buffer_unordered`, `JoinSet`, `try_join!`, cancellation tokens, task trackers, and channels where they match the lifecycle.
- Make task ownership and cancellation explicit. If a newer request supersedes an older one, preserve the existing generation/abort or cancellation check rather than allowing stale callbacks to mutate current state.
- Use snapshots or `Arc` when a background operation must observe a stable configuration. Do not introduce locks or clones without a clear ownership or consistency reason.
- Keep external API protocol details close to the client/domain module that owns them. Preserve endpoint parameters, headers, identifiers, signing rules, and special-case constants unless the task explicitly changes protocol behavior.
- Distinguish user-facing messages from internal diagnostic context. Existing language and wording should be preserved during a behavior-preserving refactor unless explicitly approved otherwise.

### Data, configuration, and tests

- Treat serialized settings, database rows, migrations, credentials, and cached data as compatibility surfaces. Use explicit `serde(default)`/normalization/versioning patterns where the existing project does so, and do not silently change old data semantics.
- Prefer small behavior-focused unit tests colocated with the module. The reference project tests parsers, conversions, status transitions, validation, scheduling, and other pure invariants; it also keeps a few explicitly ignored manual/network tests for workflows that require real services.
- Test public behavior and important state invariants rather than private helper structure. A pure helper is worth extracting when it creates a meaningful test boundary, not merely because a function is a few lines long.
- Keep test names concise and action-oriented. Use `Result<()>` in async tests when it makes error propagation clearer, and reserve `expect` for test setup or an assertion whose failure message is useful.
- For a structural refactor, first establish a baseline with the narrowest relevant checks. After each meaningful move, compile or test the affected package so file moves and behavior changes remain distinguishable.
