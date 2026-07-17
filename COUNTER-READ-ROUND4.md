# Adversarial review brief — port bible round 4 (ROD-430)

Tight re-pass on the **round-4 amend only** (K-2 / Path 3 fence + membership
stamp wording). Do not re-open dry surface from rounds 1–3 without a new
source-backed reason.

## Ground rules

1. Freeze only: zigoku `083abd3f2af6be5a372414c6b291045c926402cd` (read-only).
2. Scope = latest ROD-430 round-4 commit in sabigoku (`git log -1` / diff vs
   previous `cb12b1c` or parent).
3. Finding format: chapter+section, claim, why wrong, evidence, fix, A/B/C.
4. Per touched section: "no findings" is valid. Explicit.

## Attack surface (only this)

### 03 §5.3 K-2 + Path 3 fence

- Origin table: `forced_preferred` vs `pin_flip`. Can an implementer still unify
  exhaust handling under one flag and break pin law?
- "Clear one-shot → begin **new** non-manual full walk" vs freeze
  `beginFallback` / `advanceFallback`. Any missing step (tried mask, absence,
  stamp stays, toast copy)?
- Cross-read Path 3 (§4.1) + §5.1 pin-kept: still consistent?
- Does second open after stamp-stays still avoid the force loop?

### 02 §3.7 / L4 membership stamp

- "Stamp inside successful `recordPlay` / P / status writers" match
  `playback_session` + `store.recordPlay` + visible=false play bind?
- NOT-set-by list: sync `applyPulled`, bind-on-resolve — still accurate?
- Set-once still holds?

## Exit

Dry (no A/B) → promote chapters `review-stable`, ROD-421..429 Done, close
ROD-430 process. Residual A/B → list for author; no promote.
