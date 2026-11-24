# AGENTS.md: guide for AI agents and contributors

OpenMaker is a clean-room, open-source replacement for Claris FileMaker,
written in Rust: a desktop client compatible with FileMaker Pro 26 (its
application chrome, the Browse/Find/Layout/Preview modes, the developer
tools, local `.fmp12` files and files hosted by FileMaker Server) and
OpenMaker Server, which hosts `.fmp12` files for FileMaker Pro clients. The
goal is 1:1 compatibility. The project is **pre-alpha**; Linux (Ubuntu)
comes first, then macOS and Windows.

Read this file first, then the documents it points to.

## 1. Orientation

| Read | Why |
|---|---|
| [ROADMAP.md](ROADMAP.md) | Status by area, the alpha gate |
| [docs/architecture.md](docs/architecture.md) | Principles, crate map, seams, testing method |
| [docs/development.md](docs/development.md) | Build, run, check and debug; the research tools |
| [docs/design.md](docs/design.md) | OpenMaker's own look: the color, type and shape tokens chrome code uses |
| [docs/re/README.md](docs/re/README.md) | Index of the FileMaker specs and the findings ledger |
| The spec and feature note for your area | `docs/re/<topic>.md` says how FileMaker behaves; `docs/<feature>.md` says what OpenMaker does and its limits (both linked from ROADMAP.md) |
| Issues and pull requests for your area | `gh issue list --label "area: scripts"`, `gh pr list`: avoid duplicating work in flight |
| The crate's `lib.rs` module doc | Each crate names its spec and scope there |

## 2. Where things live

```text
crates/       the Rust workspace (25 crates; map in docs/architecture.md)
  om-gui        desktop client          om-server   OpenMaker Server
                                        om-admin    its Admin Console
                                        om-store    its store engine
  om-provider   client data access      om-data     the GUI's data contract
                                        om-report   the report renderer's interface
  om-calc, om-formula, om-scripts, om-sql, om-query, om-transfer, om-url,
  om-collate    engines
  om-hbam, om-layout                      .fmp12 files and layouts
  om-protocol, om-session, om-giop, om-scsu, om-crypto, om-rctransaction   the wire
  om-client     command-line protocol probe
  om-author     agent authoring (CLI + MCP)
docs/re/      FileMaker facts: specs, catalogs (docs/re/catalogs), recovered .proto files
docs/*.md     OpenMaker behavior per feature, with its limits; architecture; development
docs/performance/   measurements and their method
tools/        reverse-engineering and oracle tooling (not product code)
re/           research data; its captures/, extracted/ and work/ are git-ignored
website/      openmaker.dev, a static page (website/README.md)
```

## 3. Golden rules

1. **Clean room.** Implementation is written from `docs/re/` specs,
   captured traffic and observed behavior, never by copying or translating
   decompiled code. Specs describe behavior and cite evidence (symbols,
   addresses, captures, oracle runs); they do not paste decompiled bodies.
   When a spec is missing or wrong, write or fix the spec first, or file a
   `needs-re` issue. Say in the spec what is observed and what is inferred.
2. **Chrome is code, content is data.** OpenMaker replicates FileMaker
   Pro's application chrome. The layout content in the window is the
   file's own design, decoded and rendered from the file. Never hardcode a
   particular file's layouts, fields, styles or scripts in product code.
3. **Everything is a command.** Build new or changed user-visible
   behavior as an operation in the crate that owns it, not in UI code,
   with a stable id, a label (and menu path and shortcut where it has
   them), documented typed parameters, an availability check, and a
   structured result or typed error. Menus, dialogs, the CLI and MCP tools
   all call that operation; only presentation and window state stay in
   the UI. Today `om-author`'s CLI and MCP server share one `Command`
   set and `Service`, while `om-gui` dispatches its own `Action`s, which
   agents cannot reach. Add no GUI-only behavior, and move existing
   behavior behind an operation when you change it, not as a rewrite
   first. What OpenMaker cannot do yet fails explicitly (rule 4).
4. **Match FileMaker, and say when you can't.** Measure against FileMaker
   (captures, the `tools/fms-*` oracles, live probes) wherever possible.
   When behavior is inferred or OpenMaker's own choice, the doc says so and
   the work counts as unverified parity. Unsupported behavior fails the way
   FileMaker fails (for a script step, error 3), never as a silent success.
5. **Never lose data.** Codecs keep every byte they do not understand and
   re-encode unchanged structures exactly. Refuse an edit (read-only,
   error) rather than write something FileMaker might not accept.
6. **Input is hostile.** `.fmp12` files and network messages can be
   malformed or malicious: check bounds, bound recursion, cap allocations
   sized by input, and return errors instead of panicking. `unsafe` is
   forbidden workspace-wide.
7. **Secrets and lab specifics stay out of git.** Never commit captures,
   license keys, credentials, auth tokens, or the names and addresses of
   the maintainer's test servers, test files or machines. `re/captures/`,
   `re/extracted/`, `re/work/`, `.env` and `.secrets/` are git-ignored for
   this reason; keep research output inside those `re/` folders, since the
   rest of `re/` is not ignored. Mask license keys and
   tokens in anything you print. Lab details are documented outside this
   repository; if your instructions point you to a lab guide, read it
   before any live test.
8. **Be gentle with live FileMaker servers.** Use only what the lab guide
   allows, stop after a failed login instead of retrying (accounts lock
   out), and revert any server configuration you change.
9. **Pixel fidelity.** The UI is judged against FileMaker Pro 26. Look at
   the result (see [Verifying UI changes](docs/development.md#verifying-ui-changes));
   if something nearby clearly looks wrong, fix it or file it.

## 4. Tests: few and high-value

At this stage tests that do not catch real regressions slow development
down. Add one when it pays for itself:

- **Formats and protocol:** parse or round-trip real captured or sample
  bytes, or compare with oracle vectors. These are the tests that prove
  compatibility.
- **A bug that was expensive to find:** one test, at the lowest layer
  that failed, in an existing test file where one fits.
- **Data-loss paths:** saves, locks, conflicts, transactions.

Leave out tests of trivial logic, of what types already guarantee, of UI
details a screenshot shows better, exhaustive matrices, and the same case
repeated at several layers. Prefer adding a row to an existing vector table
or an assertion to an existing test. Check behavior in the running app.

Keep the existing suite green. A flaky test is fixed, or filed with the
`flaky-test` label; never retried in a loop or ignored without a reason. A
new `#[ignore]` states its inputs: `#[ignore = "requires ..."]`.

## 5. Before you finish

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test -p <crates you touched>                         # while iterating
xvfb-run -a cargo test --locked --workspace --no-fail-fast # before a pull request
```

- **UI changes:** look at the screens and keep before/after images for the
  pull request.
- **Docs:** update the `docs/re/` spec when you learned something about
  FileMaker, the feature note's limits when OpenMaker's behavior changed,
  and ROADMAP.md only when an area's status changed. Rewrite stale text in
  place; do not append a second account.
- **Gaps you found but did not fix:** file them as issues.

## 6. Issues

Work is tracked in [GitHub issues](https://github.com/OpenMaker-Dev/openmaker/issues),
not in the repository: no task lists, backlogs or plans in docs or code.

- Search first (`gh issue list --search "..."`). One problem per issue.
- In the docs, an issue number written as a link is an issue of this
  tracker. A bare number (`#1006`) is an issue or pull request number from
  the project's earlier, private tracker, kept as provenance; it does not
  refer to this repository's issues. Code comments written before the move
  cite the earlier tracker's numbers, bare. Link new references to this
  tracker's issues.
- **Title:** the problem in plain words: "Portal rows ignore the portal's
  filter on the desktop".
- **Labels:** exactly one type (`bug`, `parity-gap`, `needs-re`,
  `documentation`, `enhancement`, `performance`, `security`, `flaky-test`,
  `tech-debt`), at least one `area: ...`, and where they apply a
  `platform: ...`, status (`needs-maintainer`, `needs-live-test`,
  `parity-unverified`, `blocked`, `alpha`) and
  `priority: ...`. `gh label list` shows each label's meaning.
- **Body:** what OpenMaker does; what FileMaker does, with its evidence
  (spec section, capture, oracle vector, or "unmeasured"); where (crate,
  `path:line`, doc section); how to reproduce or check it.
- **End with the model and harness** that wrote it:
  `Filed by Claude Opus 5.5 (claude-opus-5-5) in Claude Code, via T3 Code.`

## 7. Pull requests

- One problem per branch and pull request. `main` stays green.
- **Commit subjects:** imperative, plain language, describing the result:
  "Hold ordinary save conflicts for the user's choice". Commits carry no
  AI co-author trailers.
- **Body:**
  - **Summary:** the problem and the change.
  - **Evidence:** what you ran and observed: the checks above, focused
    tests, and for UI changes before/after screenshots. "Tests pass" alone
    is not evidence.
  - **Limits:** what is not covered, and what was not compared with
    FileMaker.
  - `Closes #n` for the issues it resolves.
  - **Model and harness**, and any reviews:
    ```text
    Written by Claude Opus 5.5 (claude-opus-5-5, xhigh) in Claude Code, via T3 Code.
    Reviewed by GPT-6.1 Sol (high) in Codex, via T3 Code: 2 rounds, 3 findings fixed, approved.
    ```
- Check each review finding against the source before acting on it, and
  say in the pull request which were fixed and which were dismissed, and
  why.
- Do not merge unless the maintainer asked you to.

## 8. Parallel agents

- Work in your own git worktree and branch; each worktree builds into its
  own `target/` (several GB; remove it when the worktree is done).
- Re-read a shared file just before editing it, and keep edits to shared
  registries small. If another agent's unfinished change breaks the build,
  wait or tell the maintainer; do not fix their files.
- `cargo new` inside this workspace appends the new member to the last
  `members` line of the root `Cargo.toml`; put it on a line of its own.
- Git-ignored inputs (the `re/` data, `.env`) are not in a new worktree. Link them
  from a checkout that has them rather than copying secrets around.
- Live servers and the oracle harness container are shared: follow the
  lab guide's locking rules and expect to wait.

## 9. Documentation rules

- `docs/re/<topic>.md`: how FileMaker works (formats, protocol, behavior),
  with evidence, confidence, and an Unknowns section. A major new finding
  also gets a row in the ledger in `docs/re/README.md`.
- `docs/<feature>.md`: what OpenMaker does for a feature, the decisions
  behind it, and its limits.
- `docs/architecture.md`: design that spans crates. `docs/performance/`:
  measurements with their method.
- Code comments explain why, and the constraints the code cannot show.
- Do not append PR summaries or changelogs to docs, keep task lists there,
  or restate what the code already says. `docs/build-log.md` and
  `docs/spec-questions.md` are historical; do not add to them.
- Write plainly and precisely, with no marketing. Use `-`, not em dashes.
