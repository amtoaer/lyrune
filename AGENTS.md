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

The following summary is based on reading a shallow clone of `https://github.com/amtoaer/bili-sync`, `main` at commit `c900777e78796653fdfb00421879b6a802d1eedd`. Repeated patterns are evidence of likely preferences, not blanket rules or approval of a Lyrune architecture. The guidance below combines those observations with this repository's explicit rules; preservation requirements and GPUI-specific constraints are Lyrune requirements, not claims about bili-sync. When an inference would materially affect a design decision not covered by the developer's recorded authorization below, explain the evidence and ask the developer first.

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

## `/goal`: Behavior-preserving reorganization of Lyrune

This section supplies constraints for the current Lyrune reorganization and future `/goal` tasks; it does not itself start a goal. The goal is **refactoring only**: improve ownership, structure, readability, and removal of proven redundancy without changing existing behavior. The goal must be read together with the style rules above.

### Objective

Reorganize the Lyrune application so that code can be understood by responsibility and lifecycle instead of requiring readers to navigate a multi-thousand-line application source file. Reduce implicit state transitions, duplicated entry paths, misleading names, and logic that is only understandable through call-site archaeology.

The immediate focus is `crates/lyrune-app`, especially application state ownership and the account/login/bootstrap flow present in the agreed baseline. The refactor should make it clear which code owns:

- application/view state and the top-level lifecycle;
- settings loading, normalization, and persistence;
- credential/keychain I/O;
- account restoration, QR login, logout, and credential sessions present in the agreed baseline;
- home/library/search/playlist operations;
- playback, queue, cache, CDN, and MPRIS integration;
- rendering and event wiring.

The proposed responsibilities are candidates, not an automatic file layout. Choose boundaries from actual cohesion and dependency direction. Do not split methods only to reduce line count. Survey the repository for related ownership problems, but prioritize the application view; do not reduce the whole goal to a client-GUID fix or expand it into an unapproved rewrite of other crates.

### Baseline and PR boundaries

The developer selected `master@1320e5d` (v1.5.0) as the behavior baseline and excluded PR #35. This identifies the behavior to preserve, not a request to reset the checkout: continue from the current refactor progress. Before code changes, inspect the actual branch, HEAD, and uncommitted changes and record them separately. Do not discard or overwrite existing work merely to prepare a refactor.

Reapplying PR #35 is a separate feature phase, not implicit authorization from this refactoring goal. If its features are absent from the agreed baseline, do not add them during the refactor. If they are already present, preserve them. Once separately authorized, reapply the feature intent to the new owners rather than blindly copying old file locations. Keep structural moves, behavior-equivalent simplifications, and feature changes separately reviewable; commit or publish only when requested.

### Recorded decisions for this refactor

The following reflect explicit developer instructions, not approval inferred from earlier assistant proposals:

- **Internal architecture may change.** Preserve user behavior; source consistency is not a goal. Module boundaries, private names and paths, the size of `LyruneView`, and internal state representations such as `AccountState` may be redesigned when this improves ownership and maintainability. A prior choice of a conservative first step does not require retaining that structure indefinitely.
- **Keep changes proportional.** Architecture freedom does not waive the style, reuse-proportion, and testing rules above. Demonstrate the ownership problem or shared invariant before adding an abstraction; fewer lines or more files alone are not success criteria.
- **The protocol crate is project-internal.** `crates/qqmusic-api` is used only by Lyrune and will not be published. Replacing all workspace `HashMap` types with `hashbrown::HashMap`, including public protocol model fields, is approved. Removing NetEase support and unnecessary abstraction layers is a candidate for separate follow-up work; it is not part of this cache optimization.
- **Settings persistence may run in the background.** A single worker may receive snapshots over a FIFO channel and finish each save before starting the next. Preserve save triggers and JSON compatibility, without debouncing or coalescing requests. Normal shutdown must wait for the final save. Atomic replacement through a same-directory temporary file is approved so an interrupted write cannot truncate the committed configuration. This authorization does not extend to other persistence paths.
- **Explain necessary complexity.** Add concise comments for complex or non-idiomatic code that cannot reasonably be simplified, explaining the constraint or reason rather than restating the code.
- **Record progress locally.** Make a small local commit after each coherent, verified stage. Do not push or reapply PR #35 without separate authorization.
- **Reuse decisions, do not invent them.** Behavior, protocol, persistence compatibility, and observable timing remain protected. Ask about unresolved changes outside the authorized scope when they become concrete. An assistant's proposed architecture or `/goal` template is not itself developer approval.

### Hard constraints: no behavior change

The following are part of behavior and must remain unchanged unless the developer explicitly approves a separate product change. Internal representations and mechanisms may change only when these semantics remain equivalent:

- Network endpoints, request methods, query/form parameters, headers, signing inputs, device identifiers, and special protocol constants.
- The distinction between public client identity, ordinary playback GUIDs, encrypted playback identifiers, and CDN scheduling identifiers. Similar-looking GUIDs must not be merged merely to simplify code.
- Settings serialization, defaulting, normalization, file locations, legacy-file handling, and persistence timing.
- Credential storage format, keychain behavior, blocking-operation boundaries, and credential deletion/update semantics.
- Startup restoration, QR login, QQ quick login, logout, retry, cancellation, generation checks, and stale-task handling.
- User-observable account state transitions, loading/error states, user-visible messages, notifications, and whether a request is started automatically or only after user action.
- Playback behavior, cache behavior, queue behavior, task concurrency, request ordering where it affects results, and external process invocation.
- QQ local quick login and daily-30-track behavior if present in the agreed baseline; future PR reapplication must be validated separately against its approved feature requirements.
- Public APIs, serialized data, database schema, migration meaning, command-line behavior, and externally observable error categories.

A refactor may redesign internal ownership and state, move code, rename private items, extract calculations, or change module visibility when the resulting behavior is demonstrably identical. It must not use the refactor as an opportunity to add fallbacks, alter retries, change timing, replace error messages, or “clean up” unrelated protocol behavior.

### Required working method

1. **Establish a baseline.** Inspect the current branch, applicable instructions, tests, and the PR diff. For code changes, run the narrowest relevant `cargo test`, `cargo check`, `rustfmt --check`, and `git diff --check` commands before editing. Record commands and results; do not claim runtime/network/keychain behavior was verified if it was not.
2. **Map the existing behavior.** Before moving code, inventory constructors, event handlers, public entry points, async tasks, state enums, persistence calls, credential calls, and all callers of the affected methods. Write down the important state transitions and invariants in the task notes.
3. **Separate structural moves from behavior-equivalent simplifications.** Prefer a move/extraction checkpoint first. Only after it compiles and the relevant tests pass should the task simplify duplicated or obscure control flow, with an explicit equivalence argument. Actual behavior changes, including bug fixes, are outside this goal and require separate approval. Use separate review checkpoints, and separate commits only when committing is authorized.
4. **Move by cohesive responsibility.** Candidate modules may include account/bootstrap, playback, library/home, search, settings UI, and rendering, but the final layout must follow actual dependencies. Keep shared types and truly cross-cutting state in a small central module; do not create a second giant “context” object just to hide fields.
5. **Make lifecycle ownership explicit.** Identify the existing owner of initialization and each transition, including credential reads, client identity resolution, persistence, and UI application when present in the baseline. A login button that re-enters initialization may be an existing recovery path, not removable redundancy. Preserve it until an equivalent organization is demonstrated; if removing it changes retry behavior, describe the trade-off and seek separate approval rather than deleting it under this goal.
6. **Clarify task/UI boundaries.** Keep background work and UI result application distinguishable, preserving GPUI context, cancellation, generation, and thread-affinity requirements. This is not permission to move existing synchronous settings saves into background tasks, relocate persistence relative to state updates, or alter notification timing merely to make callbacks thinner.
7. **Remove only demonstrated redundancy.** Before deleting a state assignment, notification, guard, clone, helper, or branch, identify all call paths and explain why its invariant is now guaranteed. Code that is redundant on startup may still be required for retry, logout, stale-task, error, or test paths.
8. **Validate incrementally.** Run a compile/test check after each coherent module extraction. At the end, run formatting, diff checks, focused tests, and the relevant package check. Compare the final call graph and state transitions with the baseline rather than relying only on compilation.

### Preferred structural direction

Use the existing project's direct style rather than introducing a framework. A possible direction is:

```text
crates/lyrune-app/src/
├── app.rs                 # view type, shared state, top-level construction/render composition
├── app/
│   ├── account.rs         # credential restoration, login, quick login, logout, account state
│   ├── playback.rs        # queue, playback, quality, cache/CDN integration
│   ├── library.rs         # home, playlists, library loading and mutations
│   ├── search.rs          # search and artist flows
│   ├── settings_ui.rs     # settings event handlers and UI application
│   └── render.rs          # large render sections when they have a coherent boundary
```

This is a starting hypothesis, not a mandate. Existing standalone modules such as settings, credentials, networking, and design code should remain separate when they already have a good boundary. Do not create `service`, `manager`, `controller`, or `util` modules as dumping grounds. A module should have a name that tells a reader what behavior it owns.

For the account path, consider the following responsibility map only after checking the agreed baseline. It does not require new types, a new state model, an extra layer, or migration behavior absent from that baseline:

```text
SettingsStore       plain settings read/write
CredentialStore     credential/keychain read/write
Account bootstrap   restore + client identity resolution + persistence decision
LyruneView          apply result to AccountState and render/event state
Login operations    QR or QQ login after required identity is ready
```

Where `client_guid` is present in the agreed baseline, its internal representation may change under the architecture authorization above, but empty-vs-present semantics and all request, persistence, migration, and retry behavior must remain equivalent. Changing its authority or those semantics requires separate confirmation; a cleaner type alone does not justify a behavior change.

### What counts as a good refactor

- A reader can find the owner of a state transition without searching the whole repository.
- Each async task has an obvious owner, completion path, cancellation rule, and UI update boundary.
- Settings and credentials do not know about each other's storage mechanism unless an explicitly approved migration boundary requires it.
- Login entry points and any initialization/retry paths they invoke have explicit, understandable responsibilities. Existing retry behavior remains intact; a new failure screen or removal of automatic login is a product change, not structural cleanup.
- Repeated logic is removed only when the cases really share the same rule; superficially similar playback/CDN/device identifiers remain separate when their protocols differ.
- Helpers are extracted for a meaningful domain operation, lifecycle boundary, or focused test—not simply to make a long function look shorter.
- Comments explain non-obvious constraints and decisions, especially where code was split across modules.
- The resulting module graph is understandable and mostly one-directional; a split that merely replaces one large file with cyclic imports or a larger number of indirections is not an improvement.

### Questions requiring explicit confirmation

Apply the recorded decisions above first. Routine internal design choices within that authorization do not require renewed approval. The following checklist identifies decisions that require confirmation when they remain outside the authorized scope, affect protected behavior or compatibility, or expand the task. For such a decision, explain the concrete context, options, recommendation, and risks, then ask **before making the affected change**. Do not infer approval from silence, earlier assistant recommendations, or reference-project conventions. Ask only when the decision blocks the next stage; do not require all distant design details upfront. Listing a question in a final report is not a substitute for asking before implementation. Preserve the baseline while awaiting an answer and reuse explicit answers already given.

1. **Module granularity.** Should the application be split by domain (`account`, `playback`, `library`, `search`, `settings`, `render`) or by a smaller/different grouping? Moving methods into `app/*.rs` improves navigation, but too many modules can make GPUI state and imports harder to follow.
2. **Private `LyruneView` state.** Is it acceptable for multiple implementation modules to keep operating on one large `LyruneView`, or should state itself be grouped into substructures? The former is a lower-risk file split; the latter may improve ownership but changes access patterns and has a larger regression surface.
3. **State-model changes.** May the task replace sentinel strings, add state variants, introduce a resolved bootstrap result, or change `AccountState`? These can clarify invariants but may alter retry/error behavior even when the UI looks the same.
4. **`client_guid` representation and authority.** Should settings be authoritative after migration, should credentials retain a copy, and should an empty GUID be represented by `String`, `Option`, or a newtype? This affects legacy migration, persistence, and all login request paths.
5. **Synchronous versus asynchronous persistence.** Is it acceptable to change settings or credential saves from their current synchronous/blocking call sites to background tasks? That can improve responsiveness but changes ordering, failure timing, and what a subsequent login observes.
6. **User-visible text and diagnostics.** May error messages, login status text, tracing messages, or language consistency change? A wording cleanup is not behavior-neutral for users, tests, and support diagnosis; preserve text by default.
7. **Public/private renames and re-exports.** May private methods and module paths be renamed, or must the refactor preserve names/import paths for downstream crates and future PR rebases? Prefer private renames only when they remove real ambiguity.
8. **Shared-state abstraction.** Is introducing a small state holder or domain object allowed, and what concrete ownership problem must it solve? Do not introduce `AppContext`, `AccountManager`, or similar abstractions solely to move fields out of sight.
9. **Error type scope.** Should the refactor retain `anyhow` and existing error chains, or is a typed error boundary desired for a specific subsystem? Typed errors improve machine-readable branching but add conversion and maintenance surface.
10. **Commit and PR boundaries.** Should the structural move be a separate commit/PR from the behavior-preserving account cleanup and PR #35 feature reapplication? Separate boundaries improve review and rollback but may require temporary intermediate module visibility.
11. **Test expansion.** Which state transitions deserve new tests when existing coverage is limited? Adding focused tests for a pure invariant is usually useful; adding a large mock framework for GPUI/keychain integration is not automatically justified.
12. **Scope of adjacent cleanup.** If the agent finds unrelated dead code, naming inconsistencies, duplicated UI helpers, or protocol oddities, should they be recorded as follow-ups or included? Default to follow-up; include them only with explicit approval when the target refactor cannot be correct without them.
13. **Uncertain style inferences.** If a change depends on comment language/density, wildcard imports, `mod.rs` versus `name.rs`, file-size limits, newtypes, traits, generics, or formatter settings, show the relevant local and reference examples and ask. Preserve local conventions meanwhile; do not impose a universal rule from isolated examples.
14. **Behavior baseline and feature phase.** Which revision and local patches should the refactor preserve, and is PR #35 reapplication a later task? Recommend a confirmed baseline and a separate feature phase; including unreviewed migration/retry changes would make equivalence hard to establish.

For every remaining question that requires confirmation, the task report should include: the exact code/context, why the existing behavior does not determine the answer, the recommended option, the alternative(s), the likely diff/risk, and the question awaiting confirmation. Continue with independent, non-controversial structural work only; do not silently decide the disputed part.

### Completion requirements

Before declaring this goal complete, report:

- the final module/file responsibility map and any intentional exceptions;
- the agreed behavior baseline, which structural moves were made, and the equivalence evidence for each control-flow simplification;
- the invariants and lifecycle ownership that became explicit;
- every approved decision from the questions above and every deferred follow-up;
- focused tests/checks actually run, including failures or checks not run;
- confirmation that no product behavior, protocol value, persistence format, or user-visible state transition was intentionally changed.
