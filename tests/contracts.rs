//! Acceptance scaffold for the 05 behavior-contract inventory (ROD-440).
//!
//! This is an integration binary: it sees only sabigoku's public API, never the
//! `#[cfg(test)]` unit tests beside each module. So a `Seeded` row does not run
//! its contract here; it names the inline test that already enforces it. The
//! value this file adds is a keyed map from every 05 section to its coverage
//! disposition, and a guard that a section can never go missing in silence.
//!
//! The `cov` on each row is truth at HEAD, not the ticket's 2026-07-17 snapshot:
//! contracts shipped by any milestone (M1.3 through M1.8) read `Seeded`. Only
//! genuinely unbuilt surfaces read `Pending`. A false `Pending` on covered work
//! would be its own silence.

use sabigoku::domain::{Enrichment, ListStatus, Translation};
use sabigoku::store::Store;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cov {
    /// Enforced by the named inline unit test (`module::test`).
    Seeded(&'static str),
    /// Not yet built; the string is the M2 slice that will own it.
    Pending(&'static str),
    /// Ships as-is or lives in another chapter's tests; the string says which.
    Deferred(&'static str),
}

struct Contract {
    /// 05 subsection id, e.g. "10.3".
    section: &'static str,
    title: &'static str,
    cov: Cov,
}

const fn c(section: &'static str, title: &'static str, cov: Cov) -> Contract {
    Contract {
        section,
        title,
        cov,
    }
}

use Cov::{Deferred, Pending, Seeded};

/// The 05 inventory, one row per subsection contract cluster. Titles track the
/// chapter so 05 stays the index; cites name a representative enforcing test.
const CONTRACTS: &[Contract] = &[
    // 1 · Navigation, quit, Esc chain
    c(
        "1.1",
        "j/k clamp to bounds; g/G jump to ends; scroll follows the jump",
        Seeded("tui::view::browse::nav_clamps_and_jumps"),
    ),
    c(
        "1.1",
        "scrollIntoView keeps the cursor and its header visible",
        Seeded("tui::view::history::scroll_keeps_the_cursor_and_its_header_visible"),
    ),
    c(
        "1.2",
        "q / Ctrl-C quit; Settings saves dirty first; never back-nav",
        Seeded("tui::app::q_quits_from_every_view_in_normal_mode"),
    ),
    c(
        "1.3",
        "Esc chain matrix per DESIGN; zoom demotes to origin pane",
        Seeded("tui::app::esc_chain_table"),
    ),
    c(
        "1.4",
        "pane focus: h/l at width>=60; narrow has no second pane",
        Seeded("tui::app::h_l_toggle_panes_per_the_focus_table"),
    ),
    // 2 · History cursor and setHistory
    c(
        "2",
        "cursor follows focused show identity across reorder; filter clamps",
        Seeded("tui::view::history::cursor_follows_identity_across_reorder_and_clamps"),
    ),
    c(
        "2",
        "cursor walks group order, not store order; geometry counts headers",
        Seeded("tui::view::history::order_walks_groups_not_store_order"),
    ),
    c(
        "2",
        "filter matches any present title form; Esc resets",
        Seeded("tui::view::history::filter_matches_any_title_form_and_esc_resets"),
    ),
    // 3 · Hard delete (ROD-220)
    c(
        "3",
        "X then y deletes with cascade; only y fires; never the playing show",
        Seeded("tui::app::hard_delete_confirm_freezes_fires_and_cancels"),
    ),
    c(
        "3",
        "delete refuses the currently-playing show",
        Seeded("tui::app::delete_refuses_the_currently_playing_show"),
    ),
    c(
        "3",
        "cascade drops progress/cache/bindings/pin/absence/route",
        Seeded("store::delete_show_cascades_children"),
    ),
    // 4 · Status, undo, recompute, add (ROD-139/189/193)
    c(
        "4",
        "p/x/c/w transition status in store + memory; u undoes last",
        Seeded("tui::app::history_status_keys_transition_store_and_memory_with_undo"),
    ),
    c(
        "4",
        "r recomputes from episode_progress; recompute survives a pending undo",
        Seeded("tui::app::recompute_survives_a_pending_undo"),
    ),
    c(
        "4",
        "Browse/Discover P adds a planning entry, never a provider-only row",
        Seeded("tui::app::browse_p_saves_the_highlighted_result"),
    ),
    c(
        "4",
        "action-sync arms debounce on status/undo when connected (ROD-291)",
        Pending("M2 sync rail (ROD-448)"),
    ),
    // 5 · Layout gates
    c(
        "5",
        "too-small terminal renders a degraded frame, never exits",
        Seeded("tui::app::tiny_terminal_renders_the_degraded_frame"),
    ),
    c(
        "5",
        "pane_split_min 60; list width ~38% with 30-col floor",
        Seeded("tui::app::narrow_terminal_clamps_pane_to_list"),
    ),
    c(
        "5",
        "top bar degrades with width",
        Seeded("tui::app::top_bar_degrades_with_width"),
    ),
    // 6 · View switching
    c(
        "6",
        "F-keys/letters switch views; H is goto not toggle; search letters inert",
        Seeded("tui::app::switching_views_resets_pane_focus"),
    ),
    c(
        "6",
        "dirty Settings leave via B/H/D/F-keys persists on the way out",
        Seeded("tui::app::ctrl_c_skips_a_dirty_settings_persist_and_fkey_leave_saves"),
    ),
    // 7 · Discover
    c(
        "7",
        "entering Discover lands the feed; axis keys; error renders persistent",
        Seeded("tui::app::entering_discover_fetches_and_lands_the_feed"),
    ),
    c(
        "7",
        "P on a card saves planning; chip tracks selection; / jumps to search",
        Seeded("tui::app::discover_p_saves_the_selected_card_as_planning"),
    ),
    c(
        "7",
        "rows persist canonically (AniList id), never as provider bindings",
        Seeded("store::catalog_upsert_round_trips"),
    ),
    c(
        "7",
        "multi-axis fan-out drain balance; retention cap exhaustion",
        Pending("M2 discover feed depth (ROD-449)"),
    ),
    c(
        "7",
        "cover pump caps new fetches; prefetch next page near end",
        Pending("M2 discover feed depth (ROD-449)"),
    ),
    // 8 · Enrichment refresh (ROD-278)
    c(
        "8",
        "enrich overwrites drift, preserves user state; never mints a row",
        Seeded("store::enrichment_patch_never_touches_user_state"),
    ),
    c(
        "8",
        "confirmed no-match stamps freshness; stale total clears only on airing answer",
        Seeded("store::stale_total_clears_only_on_stamped_airing_answer"),
    ),
    c(
        "8",
        "three-state worker: transport error skips stamp and persist",
        Pending("M2 enrichment worker"),
    ),
    // 9 · Titles (ROD-205)
    c(
        "9",
        "primary title follows title_language with fallback chain",
        Seeded("domain::preferred_title_fallback_chains"),
    ),
    c(
        "9",
        "history filter searches any present title form",
        Seeded("tui::view::history::filter_matches_any_title_form_and_esc_resets"),
    ),
    // 10 · Resolve, preferred, pin, fallback, prewarm
    c(
        "10.0",
        "classification: tier-major binding beats an earlier key; unbound persists + warns",
        Seeded("resolve::classify_is_tier_major_binding_beats_earlier_key"),
    ),
    c(
        "10.0",
        "tier-A uses effective order then needs-search; existing binding wins",
        Seeded("resolve::classify_tier_a_uses_effective_order_then_needs_search"),
    ),
    c(
        "10.1",
        "history open fetches on the owning provider; pin opens only its own binding",
        Seeded("resolve::history_pin_opens_only_its_own_binding"),
    ),
    c(
        "10.1",
        "browse scroll fires zero episode fetches; detail entry lazy-loads",
        Seeded("tui::view::detail::continuous_scroll_settles_before_fetching"),
    ),
    c(
        "10.2",
        "unpinned open re-routes off stale binding to preferred (ROD-398)",
        Seeded("resolve::route_stale_forces_once_and_stamps_before_fetch"),
    ),
    c(
        "10.2",
        "settled under pref opens the binding directly, no restamp loop",
        Seeded("resolve::route_settled_opens_binding_without_restamp"),
    ),
    c(
        "10.3",
        "failed fetch hops provider-major: binding, absence, key, then search",
        Seeded(
            "resolve::fallback_walk_is_provider_major_binding_then_absence_then_key_then_search",
        ),
    ),
    c(
        "10.3",
        "fresh absence respected on a non-manual walk; transient error hops",
        Seeded("tui::episodes::transient_error_hops_without_marking_absence"),
    ),
    c(
        "10.3",
        "empty listing marks absence and walks the ladder; never binds empty (ROD-368)",
        Seeded("tui::episodes::empty_listing_marks_absence_and_walks_the_ladder"),
    ),
    c(
        "10.3",
        "exhausted walk dead-ends into no-source, frees the walk",
        Seeded("tui::episodes::exhausted_walk_dead_ends_into_no_source"),
    ),
    c(
        "10.3",
        "mapEpisodeIndex prefers raw then ordinal else null",
        Seeded("domain::map_episode_index_prefers_raw_falls_back_to_ordinal_else_none"),
    ),
    c(
        "10.3",
        "stream fail relaunches hop, one shot per provider per walk",
        Seeded("tui::app::failed_play_hops_relaunches_and_dead_ends_without_ping_pong"),
    ),
    c(
        "10.4",
        "prewarm candidates = unchecked only; results mint hidden bind/negative",
        Pending("M2 prewarm walk (ROD-449)"),
    ),
    c(
        "10.5",
        "v cycles unpinned -> each provider -> unpinned; pin keeps on miss",
        Seeded("resolve::pin_flip_probes_through_absence_and_keeps_pin_on_miss"),
    ),
    c(
        "10.5",
        "v flip keeps cursor on the in-progress episode (R-9); retired pin unpins",
        Seeded("tui::episodes::pin_cycle_sets_flips_and_clears"),
    ),
    c(
        "10.6",
        "resume target = most-recent row; failed auto-open demotes to History",
        Seeded("tui::app::last_watched_landing_demotes_when_the_walk_exhausts"),
    ),
    c(
        "10.6",
        "fires only on first history load; successful load clears the demote arm",
        Seeded("tui::app::cached_landing_clears_the_demote_arm_synchronously"),
    ),
    c(
        "10.7",
        "grid cursor seeds from progress; resume overrides; completed -> ep one",
        Seeded("tui::episodes::cursor_seeds_from_progress_resume_and_completion"),
    ),
    // 11 · Playback session
    c(
        "11",
        "position_update refreshes live fields; checkpoint ~30s",
        Seeded("player::ipc_handshake_events_and_final_position"),
    ),
    c(
        "11",
        "meaningful final persists; no observed position keeps the checkpoint",
        Seeded("player::ipc_without_meaningful_position_keeps_the_gate_shut"),
    ),
    c(
        "11",
        "partial watch records play, not progress/dim/advance (R-14)",
        Seeded("tui::workers::partial_watch_lands_in_history_without_ratchet"),
    ),
    c(
        "11",
        "completed advances cursor + dims; final episode toasts all-caught-up",
        Seeded("tui::app::finale_finish_toasts_all_caught_up"),
    ),
    c(
        "11",
        "other-show playback never advances this detail; double firePlay no-op",
        Seeded("tui::app::cross_show_finish_never_touches_the_new_detail"),
    ),
    c(
        "11",
        "landing/reroute progress joins are raise-only (02 4b / ROD-346)",
        Seeded("store::raise_to_union_never_lowers"),
    ),
    // 12 · Covers
    c(
        "12",
        "decision table: none/fetch/clear/up_to_date/suppress cooldown",
        Seeded("tui::view::detail::matching_cover_installs"),
    ),
    c(
        "12",
        "live pixels win over stale failure; cover_art off never fetches",
        Seeded("tui::view::detail::cover_art_off_never_fetches"),
    ),
    c(
        "12",
        "stale cover never installs for a moved selection; single-flight",
        Seeded("tui::view::detail::stale_cover_never_installs_for_a_moved_selection"),
    ),
    // 13 · Settings
    c(
        "13",
        "cycle presets wrap; unrecognized stored value snaps valid",
        Seeded("tui::view::settings::cycle_rows_step_their_wheels_both_ways"),
    ),
    c(
        "13",
        "mpv_path edit round-trip; empty never commits blank; edit swallows globals",
        Seeded("tui::view::settings::edit_mode_swallows_globals_and_rejects_control_chars"),
    ),
    c(
        "13",
        "translation / palette / landing live-sync on cycle",
        Seeded("tui::app::palette_cycle_projects_live"),
    ),
    c(
        "13",
        "preferred-provider wheel: unset -> names -> unset (ROD-344)",
        Seeded("tui::view::settings::provider_wheel_walks_unset_then_names_then_unset"),
    ),
    c(
        "13",
        "connect row is an action (not a cycle); side-effect inert until ROD-448",
        Seeded("tui::view::settings::connect_row_reports_the_action"),
    ),
    c(
        "13",
        "reloadAuth must not free a token still used by in-flight flush",
        Pending("M2 sync rail (ROD-448)"),
    ),
    // 14 · Toasts and async chrome
    c(
        "14",
        "topic singleton refreshes in place; persistent survives; overflow evicts oldest",
        Seeded("tui::toast::persistent_topic_refreshes_in_place"),
    ),
    c(
        "14",
        "Browse failure never marks History unavailable (T-3)",
        Seeded("tui::app::search_outage_toasts_persistently_and_recovers"),
    ),
    c(
        "14",
        "copy budget truncates with ellipsis",
        Seeded("tui::toast::copy_is_truncated_to_the_36_col_budget"),
    ),
    c(
        "14",
        "sync flush whispers; update_available low-key whisper",
        Pending("M2 sync rail (ROD-448)"),
    ),
    // 15 · Detail chrome / provider caption
    c(
        "15",
        "meta field order and ? degrade; two-column keyed on pane width (T-16)",
        Seeded("tui::view::detail::meta_fields_walk_the_priority_order"),
    ),
    c(
        "15",
        "provider caption: serving leads, markers, dim, shed order vs Pinned",
        Seeded("tui::view::detail::provider_row_composes_with_and_without_pin"),
    ),
    c(
        "15",
        "Browse preview hides stale episode grid from History (T-9)",
        Seeded("tui::app::history_pane_entry_engages_and_list_focus_hides_the_grid"),
    ),
    // 16 · Search / command input
    c(
        "16",
        "search chars append + debounce; letters do not navigate",
        Seeded("tui::app::typed_search_debounces_fetches_and_renders"),
    ),
    c(
        "16",
        "load-more fires on Down arrow at the last result (ROD-156 parity)",
        Seeded("tui::app::browse_nav_pushes_the_shared_detail"),
    ),
    c(
        "16",
        "command mode: dub toggle; unknown command flashes and toasts",
        Seeded("tui::app::unknown_command_flashes_and_toasts"),
    ),
    // 17 · Cross-cutting store contracts enforced via TUI
    c(
        "17",
        "upsert/enrich never clobbers user state",
        Seeded("store::catalog_merge_null_never_wipes"),
    ),
    c(
        "17",
        "sibling providers union into one show progress, never forked per provider",
        Seeded("store::raise_to_union_never_lowers"),
    ),
    c(
        "17",
        "catalog_cache carries Discover/Browse paint without refetch",
        Seeded("store::catalog_upsert_round_trips"),
    ),
    c(
        "17",
        "pin/absence/route off the enrichment path",
        Seeded("store::pin_and_route_round_trip"),
    ),
];

/// A 07 bug-ledger entry touching an M1 module. Every M1-relevant scar earns a
/// named regression cite or an explicit deferral note; nothing sits silent.
struct BugCheck {
    id: &'static str,
    disposition: &'static str,
    cov: Cov,
}

const fn b(id: &'static str, disposition: &'static str, cov: Cov) -> BugCheck {
    BugCheck {
        id,
        disposition,
        cov,
    }
}

const LEDGER: &[BugCheck] = &[
    b(
        "ID-1",
        "FIX-IN-RUST: anilist_id SOT, bindings are edges",
        Seeded("store::bind_mints_identity_row_without_membership"),
    ),
    b(
        "ID-2",
        "FIX-IN-RUST: durable catalog_cache, no library pollution",
        Seeded("store::enrichment_patch_never_mints_a_row"),
    ),
    b(
        "ID-3",
        "CLONE: atomic migrate ladder + busy timeout",
        Seeded("store::concurrent_openers_serialize_on_the_ladder"),
    ),
    b(
        "ID-4",
        "CLONE: propagate bind errors, never a silent NULL",
        Deferred("store bind-error injection deferred with the 439 store-failure harness"),
    ),
    b(
        "ID-5",
        "CLONE: enrich must not freeze aired-so-far as total",
        Seeded("store::stale_total_clears_only_on_stamped_airing_answer"),
    ),
    b(
        "ID-7",
        "CLONE: rusqlite bundled so stock macOS does not segfault",
        Deferred("build-time crate choice (Cargo.toml); no runtime test"),
    ),
    b(
        "K-1",
        "OPEN: resume marker one behind after source switch; ships as a limitation",
        Deferred("02 L1 string equality; repair UX is a future ticket"),
    ),
    b(
        "K-2",
        "FIX-IN-RUST: search-only preferred miss falls back, no blank dead-end",
        Seeded("resolve::forced_preferred_miss_triggers_k2_continuation_not_dead_end"),
    ),
    b(
        "R-1",
        "CLONE: preferred/pin/fallback/empty/demote matrix",
        Seeded(
            "resolve::fallback_walk_is_provider_major_binding_then_absence_then_key_then_search",
        ),
    ),
    b(
        "R-2",
        "CLONE: manual flip to empty keeps pin, falls back, names the miss",
        Seeded("tui::episodes::pin_flip_miss_keeps_pin_and_grid"),
    ),
    b(
        "R-3",
        "CLONE: backup-only show walks providers before giving up",
        Seeded("tui::episodes::empty_listing_marks_absence_and_walks_the_ladder"),
    ),
    b(
        "R-4",
        "CLONE: empty listing walks the ladder, never bound as success (ROD-368)",
        Seeded("tui::episodes::empty_listing_marks_absence_and_walks_the_ladder"),
    ),
    b(
        "R-5",
        "CLONE: stream open / CDN block retries then toasts",
        Seeded("player::open_failed_retries_with_backoff_then_succeeds"),
    ),
    b(
        "R-6",
        "CLONE: playback failure surfaces an error, never aborts",
        Seeded("player::other_exit_codes_fail_without_retry"),
    ),
    b(
        "R-7",
        "CLONE: softsubs fetch / retry / content-based track pick",
        Deferred("M2 subtitle pipeline; not ported in M1"),
    ),
    b(
        "R-8",
        "FIX-IN-RUST: cap is one shared HLS seam every provider routes through, no fallback-only path to drop it",
        Seeded("providers::hls::select_variant_cap_policy_picks_the_right_rung"),
    ),
    b(
        "R-9",
        "CLONE: provider flip keeps the in-progress episode cursor",
        Seeded("tui::app::v_cycles_the_pin_with_toasts_and_flip"),
    ),
    b(
        "R-10",
        "CLONE: SSRF / unsafe link check on all resolve paths",
        Seeded("player::private_stream_url_is_blocked_before_spawn"),
    ),
    b(
        "R-11",
        "CLONE: post-play refresh follows the watched show",
        Seeded("tui::app::cross_show_finish_never_touches_the_new_detail"),
    ),
    b(
        "R-12",
        "CLONE: still-airing show never auto-completes",
        Seeded("domain::after_play_still_airing_never_auto_completes"),
    ),
    b(
        "R-13",
        "CLONE: progress unclamped in store; clamp is render-time only",
        Seeded("store::progress_storage_is_unclamped"),
    ),
    b(
        "R-14",
        "CLONE: partial (0.80 natural end) vs fully_watched (0.95)",
        Seeded("domain::natural_end_is_the_080_tier"),
    ),
    b(
        "T-1",
        "CLONE: superseded episode prefetch detached, not joined",
        Seeded("tui::episodes::superseded_result_is_dropped_not_installed"),
    ),
    b(
        "T-2",
        "CLONE (intent): quit drains workers without deadlock; times out instead of hanging",
        Seeded("tui::workers::drain_times_out_instead_of_hanging"),
    ),
    b(
        "T-3",
        "CLONE: Browse task_error never marks History unavailable (ROD-234)",
        Seeded("tui::app::search_outage_toasts_persistently_and_recovers"),
    ),
    b(
        "T-4",
        "CLONE: Discover fetch off-thread with deadlines",
        Deferred("M2 discover feed depth (ROD-449)"),
    ),
    b(
        "T-5",
        "CLONE: Discover covers survive relative URL / WebP",
        Deferred("M2 discover covers"),
    ),
    b(
        "T-6",
        "CLONE->FIX-IN-RUST: Discover links canonically (AniList id), not by re-title-match",
        Seeded("store::catalog_upsert_round_trips"),
    ),
    b(
        "T-7",
        "CLONE: Discover axis cycle overflow wraps",
        Seeded("tui::app::discover_axis_keys"),
    ),
    b(
        "T-8",
        "CLONE: Discover over-fetch cap on large monitors",
        Deferred("M2 discover feed depth (ROD-449)"),
    ),
    b(
        "T-9",
        "CLONE: episode grid does not bleed Browse preview from History (ROD-222)",
        Seeded("tui::app::history_pane_entry_engages_and_list_focus_hides_the_grid"),
    ),
    b(
        "T-10",
        "CLONE: watched-dim seeds from store for Browse, not only History",
        Seeded("tui::episodes::cursor_seeds_from_progress_resume_and_completion"),
    ),
    b(
        "T-11",
        "CLONE: long episode grid never strays a wrong number",
        Seeded("tui::view::detail::grid_cell_text_shapes_and_strips_control_bytes"),
    ),
    b(
        "T-12",
        "CLONE: CJK wrap never splits codepoints; center accounts for wide cols",
        Deferred("M2 CJK text measure (render layer)"),
    ),
    b(
        "T-13",
        "CLONE: Kitty acks drain and stay quiet",
        Deferred("conditional on the Kitty cover path, not the M1 halfblock default"),
    ),
    b(
        "T-14",
        "CLONE (intent): cover decode peak memory kept under a ceiling",
        Seeded("tui::view::detail::cover_cap_protects_the_grid_at_the_worst_case"),
    ),
    b(
        "T-15",
        "CLONE: history filter matches all title forms, not only romaji",
        Seeded("tui::view::history::filter_matches_any_title_form_and_esc_resets"),
    ),
    b(
        "T-16",
        "CLONE: two-column detail measured on the pane, not the terminal",
        Seeded("tui::view::detail::meta_fields_walk_the_priority_order"),
    ),
    b(
        "T-17",
        "CLONE: history detail grid renders on first focus at any width",
        Seeded("tui::app::history_pane_entry_engages_and_list_focus_hides_the_grid"),
    ),
    b(
        "A-1",
        "CLONE (must-not): auth refuses control-byte tokens",
        Pending("M2 auth module (ROD-448)"),
    ),
    b(
        "A-2",
        "CLONE (must-not): verify Viewer before persisting the token",
        Pending("M2 auth module (ROD-448)"),
    ),
    b(
        "A-3",
        "CLONE (must-not): pull-then-push, never wipe AniList on first sync",
        Pending("M2 sync rail (ROD-448)"),
    ),
    b(
        "A-4",
        "CLONE: CAS / contended skip on mid-edit sync",
        Pending("M2 sync rail (ROD-448)"),
    ),
    b(
        "A-5",
        "CLONE (must-not): reloadAuth never frees a token mid-flush",
        Pending("M2 sync rail (ROD-448)"),
    ),
    b(
        "STORE-FAIL",
        "injected store write/open failure paths (deferred at 439 review)",
        Deferred(
            "ROD-440 inherits: no fault-injection seam yet; own it in an M2 store-hardening slice",
        ),
    ),
];

// --- Guards: the inventory can never lose a section or a disposition silently.

/// Every 05 section 1..=17 appears at least once. A deleted cluster fails here
/// instead of vanishing without a trace.
#[test]
fn every_05_section_is_present() {
    for n in 1..=17 {
        let prefix = n.to_string();
        let present = CONTRACTS
            .iter()
            .any(|c| c.section == prefix || c.section.starts_with(&format!("{prefix}.")));
        assert!(present, "05 section {n} has no contract row");
    }
}

/// Cites look like a test path; Pending/Deferred notes are never blank. This
/// cannot prove a cite resolves to a real `#[test]` (the integration binary
/// can't see inline unit tests), so a wrong cite still passes here: spot-check
/// cites on review against the actual test names, that guard is human.
#[test]
fn dispositions_are_well_formed() {
    for c in CONTRACTS {
        match c.cov {
            Seeded(cite) => assert!(
                cite.contains("::"),
                "{} '{}' cite is not a test path: {cite:?}",
                c.section,
                c.title
            ),
            Pending(why) | Deferred(why) => assert!(
                !why.trim().is_empty(),
                "{} '{}' has an empty note",
                c.section,
                c.title
            ),
        }
    }
    for e in LEDGER {
        match e.cov {
            Seeded(cite) => assert!(cite.contains("::"), "{} cite is not a test path", e.id),
            Pending(why) | Deferred(why) => {
                assert!(!why.trim().is_empty(), "{} has an empty note", e.id)
            }
        }
    }
}

/// No duplicate (section, title) or ledger id: a row is one contract, once.
#[test]
fn rows_are_unique() {
    let mut seen = std::collections::HashSet::new();
    for c in CONTRACTS {
        assert!(
            seen.insert((c.section, c.title)),
            "duplicate contract {} '{}'",
            c.section,
            c.title
        );
    }
    let mut ids = std::collections::HashSet::new();
    for e in LEDGER {
        assert!(ids.insert(e.id), "duplicate ledger id {}", e.id);
    }
}

/// The pending marker the ticket asks for: `cargo test -- --nocapture` prints
/// exactly what is not yet built, so absence reads as a line, not as silence.
#[test]
fn coverage_report() {
    let count = |pred: &dyn Fn(&Cov) -> bool| CONTRACTS.iter().filter(|c| pred(&c.cov)).count();
    let seeded = count(&|c| matches!(c, Seeded(_)));
    let pending = count(&|c| matches!(c, Pending(_)));
    let deferred = count(&|c| matches!(c, Deferred(_)));
    eprintln!("05 contracts: {seeded} seeded, {pending} pending, {deferred} deferred");
    for c in CONTRACTS {
        if let Pending(why) = c.cov {
            eprintln!("  PENDING §{}: {} -> {why}", c.section, c.title);
        }
    }
    for e in LEDGER {
        if let Pending(why) | Deferred(why) = e.cov {
            let tag = if matches!(e.cov, Pending(_)) {
                "PENDING"
            } else {
                "DEFER"
            };
            eprintln!("  {tag} {} [{}]: {why}", e.id, e.disposition);
        }
    }
}

// --- Live anchors: these drive the domain-through-store public API end to end,
// so the harness executes real code, not only the registry. They overlap the
// store's own inline tests by design (belt-and-suspenders at the crate edge).

fn show(anilist_id: i64, total: u32, status: &str) -> Enrichment {
    Enrichment {
        anilist_id,
        title_romaji: format!("show {anilist_id}"),
        total_episodes: Some(total),
        status: Some(status.to_string()),
        ..Default::default()
    }
}

/// R-12 / ROD-296 across domain::after_play and store::record_play: a play that
/// reaches the last aired episode of a still-airing show must not auto-complete.
#[test]
fn still_airing_never_auto_completes_via_record_play() {
    let store = Store::open_memory().unwrap();
    store
        .add_to_library(&show(101, 12, "RELEASING"), 100)
        .unwrap();
    store.record_play(101, 12, true, 200).unwrap();
    let s = store.get_show(101).unwrap().unwrap();
    assert_eq!(s.progress, 12, "ratchet still tracks the watched episode");
    assert_ne!(
        s.list_status,
        ListStatus::Completed,
        "airing never auto-completes"
    );
}

/// The finished counterpart: same play on a FINISHED show at total does settle
/// Completed, proving the airing guard is the only thing holding it open.
#[test]
fn finished_show_completes_at_total_via_record_play() {
    let store = Store::open_memory().unwrap();
    store
        .add_to_library(&show(102, 12, "FINISHED"), 100)
        .unwrap();
    store.record_play(102, 12, true, 200).unwrap();
    assert_eq!(
        store.get_show(102).unwrap().unwrap().list_status,
        ListStatus::Completed
    );
}

/// R-14 / ROD-168 across domain::natural_end and store::record_finish: a watch
/// below the 0.80 natural end records the play but never ratchets progress.
#[test]
fn partial_watch_records_play_without_ratcheting_progress() {
    let store = Store::open_memory().unwrap();
    store
        .add_to_library(&show(103, 12, "FINISHED"), 100)
        .unwrap();
    store
        .record_finish(103, Translation::Sub, "3", 3, 30.0, 100.0, None, 200)
        .unwrap();
    let s = store.get_show(103).unwrap().unwrap();
    assert_eq!(s.play_count, 1, "the play is recorded");
    assert_eq!(s.progress, 0, "0.30 is below natural end: no ratchet");
    assert_ne!(s.list_status, ListStatus::Completed);
}

/// 02 §4b raise-only across the completion status and a later union join: a
/// force-completed show survives a landing that reports a lower union progress.
#[test]
fn completion_survives_a_lower_union_join() {
    let store = Store::open_memory().unwrap();
    store
        .add_to_library(&show(104, 12, "FINISHED"), 100)
        .unwrap();
    store
        .set_list_status(104, ListStatus::Completed, 200)
        .unwrap();
    assert_eq!(store.get_show(104).unwrap().unwrap().progress, 12);
    store
        .save_progress(104, Translation::Sub, "1", 100.0, 100.0, None, 300)
        .unwrap();
    assert_eq!(
        store
            .raise_progress_to_union(104, Translation::Sub)
            .unwrap(),
        12
    );
    let s = store.get_show(104).unwrap().unwrap();
    assert_eq!(
        s.progress, 12,
        "union of a single ep must not lower a completed show"
    );
    assert_eq!(s.list_status, ListStatus::Completed);
}
