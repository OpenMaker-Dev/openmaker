# Contributing to OpenMaker

OpenMaker is pre-alpha and moves quickly. Issues, specifications, fixes and
measurements against FileMaker are all welcome. [AGENTS.md](AGENTS.md) holds
the project's rules and workflow; it is written for AI agents and applies to
people too. This file covers what an outside contributor needs on top of it.

## Clean-room rules

OpenMaker must stay free of Claris's code and content. A contribution that
breaks these rules is refused, however useful it is.

- **No copied or translated code.** Implementation is written from the
  specifications in [`docs/re/`](docs/re/README.md), from captured traffic
  and from observed behavior. Never paste or paraphrase decompiled or
  disassembled code from FileMaker binaries, in code or in a spec.
- **Specs describe behavior and cite evidence.** A spec says what FileMaker
  does, names its evidence (symbols, addresses, captures, oracle runs), and
  says what is observed and what is inferred. If the spec you need is
  missing or wrong, fix the spec first or open a `needs-re` issue.
- **Ship nothing Claris made.** Do not add FileMaker binaries, help text,
  starter solutions, templates, themes, icons, images or sample files to the
  repository. Name, signature and value tables that compatibility needs are
  data; prose copied from Claris's documentation is not.
- **Your own obligations.** If you have had access to FileMaker's source
  code, or are bound by an agreement with Claris that restricts this kind of
  work, do not contribute to the areas it covers. Do your research within
  the license terms that bind you.

## Private data

Never attach credentials, license keys, auth tokens, network captures, or
`.fmp12` files and screenshots that hold real people's or customers' data.
Do not name private servers, hosts, IP addresses or machines. Use synthetic
data (FileMaker's starter solutions with made-up records work well) and mask
keys and tokens in anything you paste. `re/captures/`, `re/extracted/`,
`re/work/` and `.env` are git-ignored for this reason.

## Issues

Search first, then open one issue per problem with the matching template.
Say what OpenMaker does, what FileMaker does and how you know, and how to
reproduce it. Labels and wording are in [AGENTS.md](AGENTS.md), section 6.
Report security problems privately, as [SECURITY.md](SECURITY.md) says, not
in an issue.

In the docs, an issue number written as a link
(`[#186](https://github.com/OpenMaker-Dev/openmaker/issues/186)`) is an
issue of this tracker. A bare number (`#1006`) is an issue or pull request
of the project's earlier, private tracker that was not carried over here,
kept as provenance; it does not refer to this repository's issues. Code
comments written before the move cite the earlier tracker's numbers, bare.

## Pull requests

- One problem per branch and pull request, with a plain-language,
  imperative title ("Hold ordinary save conflicts for the user's choice").
- Run the checks in [AGENTS.md](AGENTS.md), section 5, and say in the
  description what you ran and what you saw. For UI changes, include
  before/after screenshots next to FileMaker Pro 26.
- Fill in the template's Summary, Evidence and Limits. Limits says what the
  change does not cover and what was not compared with FileMaker.
- If an AI agent wrote or helped write the change, name the model and
  harness at the end of the description (AGENTS.md, section 7).
- Build, run and debugging notes are in
  [docs/development.md](docs/development.md).

## License

OpenMaker is released under the [MIT license](LICENSE). By submitting a
contribution you agree that it is licensed under the same terms.
