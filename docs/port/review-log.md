# Port bible review log (ROD-430)

Adversarial loop record for the sabigoku port bible. Baseline: zigoku frozen at
`083abd3f2af6be5a372414c6b291045c926402cd` (`v0.4.6-3`, tip ROD-419). Chapters
ROD-421..429; process ROD-430. Round briefs live at the repo root as
`COUNTER-READ-ROUND*.md`; full per-round findings are comments on the Plane
tickets. This file is the durable summary.

## Round 1 — cold read + source verification (commit `4615d7a`)

Cold read of all nine docs with source closed (8 A / 12 B / 9 C findings), then
six verification agents claim-checked every chapter against the freeze, plus an
inline-comment landmine sweep. Amendments landed in one commit. Headlines:

- **02**: library membership made explicit (`library_added_at`, lock L4) after
  the cold read showed FK-parented policy tables re-created History pollution by
  construction; upsert user-state rule corrected from "COALESCE" to
  SET-clause exclusion; new §4b progress arithmetic (positional high-water,
  display-only clamp, threshold table).
- **03**: classifier rewritten as the two real entry paths (no unified
  classifier exists in zigoku); K-2 post-miss law specified; fetch-guard (SSRF)
  section; mpv argv + push-based IPC; prewarm ring corrected.
- **04**: quit contract inverted back to reality (`_exit(0)` is production;
  drains are test-only); quitFlush + pool-independent timeout; all missing
  shipped constants; toast rules.
- **05**: zero fabricated / zero misrepresented cites confirmed; added the
  uncited ROD-327/328/329 cluster, cursor-seeding cluster, three-state enrich.
- **06**: sync entry-point table (pull-first is structural); total reconcile
  matrix; new AniList-client section (one fieldset, 26/20 page sizes, no
  non-sync 429 handling at freeze).
- **07**: three missed scars (macOS SQLite segfault, v0.2.1 pane clipping and
  double-press); K-1 later relabeled. **08**: verified clean, zero corrections.

## Round 2 — author counter-read of round 1 (commit `cb12b1c`)

- Membership trigger corrected: any **meaningful play**, not "completed play"
  (zigoku: partial watches appear in History).
- 03 §4.1 Path 2 expanded to the full History-open algorithm including the
  unpinned ROD-398 re-route.
- K-2: exit the one-shot walk then full ordered walk; stamp stays on miss.
- 06 §5.4: snapshot rebaseline is pair-based (status or progress), including
  first contact.
- Test count corrected to 758 named.

## Round 3 — adversarial pass on round 2 (findings only, no commit)

1 A / 4 B / 2 C. The A: 02's "any status mutation" membership trigger combined
with an unscoped pull candidate set would let an AniList pull stamp membership
on identity-only rows (auto-import of every probed show); zigoku scopes pull to
`history_visible != 0`. The B's: missed-preferred must enter the K-2
continuation walk marked tried; pin-flip vs forced-preferred walks need an
explicit origin discriminator (identical `.manual` walks in zigoku); Path 2
unbound-arm gloss omitted the resume-demote disarm and walk cancel; 02 §4b
recordPlay row missed the membership side effect. C: count is 758 named + 1
aggregator block; applyPulled atomicity note.

## Round 4 — author amend + re-pass (commits `fd3177b`, this commit)

`fd3177b`: walk **origin** tag (`forced_preferred` | `pin_flip`) with split
exhaust behavior (K-2 six-step law; pin law untouchable); membership stamped
set-once **inside** the writers (P/reveal, user status writers, successful
`recordPlay`), with sync `applyPulled` and bind-on-resolve explicitly excluded.

Re-pass verdict: both amended surfaces **dry** against source (recordPlay
visibility promotion, walk construction sites, stamp-stays loop check all
verified). This commit closes the round-3 mechanical residue: 06 §5.4 candidate
scoping, Path 2 unbound-arm resets, 02 §4b recordPlay row, count wording,
manual-vs-origin terminology note (03 §5.2).

## Outcome

All chapters `review-stable`; ROD-421..429 Done; ROD-430 Done. Two design
decisions ratified by surviving the loop: the explicit `library_added_at`
membership column (02 L4) and cloning zigoku's Browse/History pin asymmetry with
an origin-tagged walk (03 §4.1/§5.3). Parent ROD-420 closes on Rod's final
sign-off. Post-freeze zigoku fixes follow the upstream-delta rule (PORT.md,
07 §9); new review rounds append here.
