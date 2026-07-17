# Counter-read brief — port bible round 2 (ROD-430)

You wrote the original bible chapters (ROD-421..429). Round 1 (cold read + six
source-verification agents + an inline-comment sweep, all against zigoku
`083abd3f2af6be5a372414c6b291045c926402cd`) amended them in sabigoku commit
`4615d7a`. Your job now is the counter-read: attack the **amendments**, not your
original text. The original claims were already verified claim-by-claim; round 1
findings and per-chapter change lists are in the Plane comments on ROD-421..429
and ROD-430.

## Ground rules

1. Verify against zigoku at the freeze rev only (`git -C ~/src/zig/zigoku
   rev-parse HEAD` must equal `083abd3…`). Read-only.
2. Scope = round-1 diff (`git show 4615d7a` in sabigoku). Re-litigating unchanged
   text needs a new source-backed reason, not taste.
3. A finding must name: chapter + section, the amended claim, why it is wrong or
   implementer-misleading, evidence (zigoku file:line or internal contradiction),
   and a proposed fix. Severity: A = would mislead an implementer, B = rework/
   ambiguity, C = nit.
4. Per chapter, "no findings" is a valid and useful verdict. Say it explicitly.

## Attack hardest: the two design leans (Rod has veto)

**Lean 1 — 02 §3.7 / L4: `library_added_at` as the explicit membership marker.**
The round-1 hole: policy tables FK to `show`, tier-A resolve mints bindings, so a
grid open would have created a History row. The lean keeps the FKs and adds a
nullable membership column. Attack: does any contract in 05 or flow in 03/04
still create membership implicitly? Does "completed play sets membership" clash
with any test (e.g. playing from a Browse detail without adding)? Is there a
simpler shape we both missed (FK-less policy tables, status-driven membership)?

**Lean 2 — 03 §4.1: CLONE the Browse/History pin asymmetry.**
zigoku has no unified classifier: pin = ordering preference on canonical opens,
hard restriction on History opens. Round 1 chose to document and clone the split.
Attack: does any 05 §10.5 contract contradict the split as now written? Would a
unified pin-strict rule actually break a cited test, or was cloning the timid
choice? Is the Path 2 fallback ("reopens the record's existing provider")
described precisely enough to implement?

## Per-chapter: what changed, what to check

| Chapter | Amended | Attack questions |
|---|---|---|
| PORT.md | scale note, round-1 stanza | trivial; confirm counts (321 / 759) |
| 01 | diagram replaced by import-edge table | is the table right? (`grep -n '@import' src/*.zig src/tui/*.zig`); any edge missing or invented? |
| 02 | §3.7 membership; §4 expected_episode_count branches; §4b progress arithmetic + threshold table; §5 upsert exclusion row, delete retag, new rows (cover GLOB, runtime migrate check, WAL retry, heal TTLs) | transcription drift is the main risk: check §4b against `store.zig` `unionHighWater`/`recordPlay`/`recomputeProgress` and the threshold table against `WATCHED_RATIO`/`NATURAL_END_RATIO` call sites; check expected_episode_count branches against `domain.zig:88-102` |
| 03 | §4.1 two-path classifier; §5.3 forced-preferred-miss rule (K-2 fix); §6.3.1 mpv argv + IPC; §6.5 prewarm ring; §6.7 fetch guard; §8.1/8.2 softsub; §8.3 crypto; §9 aniskip strings | check mpv argv table against `player.zig:167-208`; fetch guard rules against `util/fetchguard.zig`; K-2 fix rule: is the specified fallback complete (absence marking? stamp already advanced?) or does it leave a new ambiguity? |
| 04 | §3 quit contract inversion; §5/§5.1 drain wording; §7.3 cache races; §7.5 axes; §7.6 prewarm; §8 constants + toasts; §11 quitFlush/timeout/Kitty; §12 double-buffer | check every constant against source (`workers.zig`, `app.zig`, `input.zig:561`, `prewarm_state.zig:18`); is the quit-contract paragraph faithful to `app.zig:296-318`? |
| 05 | §4 keymap; §5 too-small wording; §7 provider-keyed P row; §8 three-state enrich; §10.0 new cluster; §10.1 P-add row; §10.7 cursor seeding; §11 raise-only; §16 rows; §18 counts | do the new contract rows paraphrase their cited tests faithfully? (spot-check §10.0 against `app_test.zig:3761-4160`, §10.7 against `3376-3549`); does the three-state enrich table match `anilist.zig:167-171`? |
| 06 | §3.3 CR/LF why; §4.4 deadline + IPv4; §5.2 entry-point table; §5.3 non-null id + no_link; §5.4 reconcile matrix + MLC cap; §5.5 CLI switch fact; §7 flags/query/exit codes; §8 env; §8b AniList client | the reconcile matrix is the highest-risk transcription: check all five cells + the snapshot-rebaseline rule against `sync.zig:199-215,288-289` and `store.zig:1160-1190`; check the entry-point table against `app.zig:1004-1040,1058-1064,1198-1209`, `workers.zig:702-737`, `main.zig:210-254` |
| 07 | ID-7, T-16, T-17, 0.2.1 index row, K-1 relabel, K-2 pointer | are the three new rows faithful to their CHANGELOG entries? Is the K-1 `OPEN` relabel right, or do you read the 02 L1 plan as a genuine fix? |
| 08 | amend-log row (owns config-format decision) | trivial |

## Also fair game

- Cross-chapter contradictions **introduced by** round 1 (e.g. §3.7 membership
  rules vs 05 §10.4 prewarm "hidden bind" language; 02 §4b vs 03 §7 thresholds).
- Round-1 omissions: something the verification reports surfaced (see Plane
  comments) that the amendments then failed to write in.

## Exit condition

Both sides dry = chapters promote to review-stable and ROD-421..429 go Done.
Findings go to Rod as a list in the format above; amendments land as a round-2
commit tagged ROD-430.
