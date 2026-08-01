# Contributing

Please read this before opening a pull request. It will save you real time.

## What this repo is

sabigoku is a personal project, not a community one. The README is upfront
about how the code gets written: what is not delegated is the judgment. I plan
it, I review every line before it lands, and I decide what the thing becomes.
That discipline is the reason the repo exists, and it is what shapes the rest of
this document.

## What I will merge

- Typo and wording fixes
- Documentation corrections
- CI, packaging, and workflow config fixes

Small, self-contained, and outside `src/`. Open a pull request directly, no
issue needed first.

## What I will not merge

Anything under `src/`: features, refactors, new dependencies, new modules,
behavior changes. However well written, however well tested, however closely it
follows the existing patterns. Those get closed unmerged.

That is not a judgment on the code. Work I did not design and did not review on
the way in is work I do not own, and ownership is the whole point here.

Found a real bug? Open an issue. I would rather fix it myself than merge a fix.

## Scope

sabigoku is a browser and a player. Keeping it to that is deliberate, and
downloading episodes to disk sits outside it. I am not taking proposals on that
one.

Feature requests generally: an issue is fine, but read it as a suggestion to
someone else's personal project rather than a roadmap item. Most get declined,
and I would rather decline quickly than leave you waiting.

## Filing a useful issue

Include:

- What you did, what you expected, what actually happened
- The exact error text or toast, copied out rather than paraphrased
- OS, terminal emulator, and `sabigoku --version`
- `sabigoku paths` output if it looks like a config or storage problem

Please do not paste stream URLs into issues, or name specific streaming sources.
The symptom and the error text are what I need to debug it.

## License

sabigoku is GPL-3.0-or-later. Anything you contribute ships under the same
license.
