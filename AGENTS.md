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
