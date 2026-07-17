# Adversarial review brief — port bible round 3 (ROD-430)

Round 1 amended the bible against zigoku `083abd3`. Round 2 was the author's
counter-read of that diff; amendments landed as the commit that added this brief
(message tags ROD-430). Your job: cold adversarial pass on the **round-2
amendments only**. Do not re-litigate unchanged text without a new source-backed
reason.

## Ground rules

1. Verify against zigoku at the freeze rev only (`git -C ~/src/zig/zigoku
   rev-parse HEAD` must equal `083abd3f2af6be5a372414c6b291045c926402cd`).
   Read-only on zigoku.
2. Scope = round-2 diff in sabigoku (`git log -1 --oneline` should be the
   ROD-430 round-2 commit; `git show HEAD` or the range since `4615d7a`).
3. A finding must name: chapter + section, the amended claim, why it is wrong or
   implementer-misleading, evidence (zigoku file:line or internal contradiction),
   and a proposed fix. Severity: A = would mislead an implementer, B = rework/
   ambiguity, C = nit.
4. Per chapter, "no findings" is a valid and useful verdict. Say it explicitly.
5. You are not the author of these amendments. Assume the author is defensive;
   attack the text as if you will implement from it tomorrow.

## What round 2 changed (attack surface)

| ID | Chapter | Change |
|---|---|---|
| A1 | 02 §3.7 / L4 / §3.5 promote | Membership trigger: **any meaningful play** (`recordPlay`, finite pos > 0), not "completed play". `completed` = progress ratchet only. |
| A2 | 03 §4.1 Path 2 | History open rewritten as full algorithm: unbound clear → pin hard bind → else unpinned `routePreferred` → else `rec.source`. |
| B1 | 03 §5.3 K-2 | On forced-preferred one-shot miss: exit single-provider walk, full ordered walk (bindings first); stamp stays; absence rules unchanged; distinct toast. |
| B2 | 06 §5.4 | Snapshot rebaseline when remote **pair** ≠ snapshot (status or progress), incl. first contact and progress-only. |
| C1 | PORT.md / 05 §18 | Test count **758** (321 in `app_test.zig`). |

## Attack hardest

### Membership (A1)

- Does any other chapter still say "completed play" sets membership / History?
- Does "meaningful play" match `PositionUpdate.isMeaningful` and every
  `recordPlay` call site (incl. play_error paths)?
- Can grid open / prewarm / enrich still stamp `library_added_at` via a side
  path the NOT-set-by list missed?
- Does "any status mutation" over-include (e.g. sync `applyPulled` on a
  non-library identity row)? Should membership be set-once-only with an explicit
  writer list?

### History open Path 2 (A2)

- Diff the pseudocode against `fireEpisodesForHistoryRecord` + `routePreferred`
  line-by-line (`resolve.zig` @ freeze). Any branch missing (retired pin,
  pin == rec.source, settled pref, stamp miss fallthrough)?
- Does the prose about pin hard-restriction contradict Path 1, 05 §10.2, or
  05 §10.5?
- Would an implementer still skip ROD-398 after reading only §4.1?

### K-2 post-miss (B1)

- Is "exit one-shot → full ordered walk" implementable without re-entering the
  blank-grid loop? Stamp stays: does a second open still behave?
- Interaction with Path 3 manual pin flip: must **not** get the K-2 continue-walk
  (pin-kept miss is intentional). Is that fence explicit enough?
- Absence: any way the new walk double-marks or skips §4.3?

### Sync snapshot (B2)

- Check `snap_changed` / `applyPulled` in `sync.zig` + `store.zig` against the
  new sentence. Any cell of the status matrix now misread because rebaseline is
  pair-based?

### Counts (C1)

- Re-count `test "` under freeze `src/`. Still 758 / 321?

## Also fair game

- Cross-chapter contradictions **introduced by** round 2.
- Round-2 omissions: something the counter-read named that the amend then failed
  to write (or wrote half of).

## Exit condition

If this pass is dry (no A/B implementer-misleading holes): promote chapters to
`review-stable` and ROD-421..429 to Done; close ROD-430 process.

If findings remain: list them in the format above for the author; further amends
land as another ROD-430 commit. Do not promote on residual A/B.
