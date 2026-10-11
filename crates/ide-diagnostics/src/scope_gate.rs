//! Applies the configured [`base_db::AnalysisScope`] (vendor-diff filter) to
//! the final diagnostics pipeline: a file entirely outside the scope skips
//! analysis before any handler runs (file-gate), and individual diagnostics on
//! unchanged lines are dropped in the finalization, immediately before inline
//! suppression (line-gate).
//!
//! Before, not after: suppression both hides findings and adds its own
//! meta-diagnostics about broken directives (`suppression.rs`), and those must
//! not be gated — their subject is the directive, not the line, which is why
//! directives themselves cannot suppress them and the baseline protects them
//! too (github#61). Running the gate first means they are born out of its
//! reach.

use std::path::PathBuf;

use crate::{config::DiagnosticsConfig, Diagnostic};

/// File-gate: whether the file participates in analysis at all.
pub(crate) fn file_in_scope(
    db: &dyn ide_db::RootDatabase,
    file_set: Option<&vfs::file_set::FileSet>,
    file_id: vfs::FileId,
    config: &DiagnosticsConfig,
) -> bool {
    let Some(scope) = &config.scope else { return true };
    match resolve_path(db, file_set, file_id) {
        Some(path) => scope.is_file_in_scope(&path),
        // No resolvable path (in-memory fixtures): conservatively in scope.
        None => true,
    }
}

/// Line-gate: drop diagnostics whose lines fall outside the scope.
/// Returns `true` when it removed anything, so the caller re-normalizes.
pub(crate) fn apply(
    db: &dyn ide_db::RootDatabase,
    file_set: Option<&vfs::file_set::FileSet>,
    file_id: vfs::FileId,
    config: &DiagnosticsConfig,
    diags: &mut Vec<Diagnostic>,
) -> bool {
    let Some(scope) = &config.scope else { return false };
    if diags.is_empty() {
        return false;
    }
    let Some(path) = resolve_path(db, file_set, file_id) else { return false };

    let text = db.file_text(file_id);
    let line_index = line_index::LineIndex::new(&text);

    let before = diags.len();
    diags.retain(|d| {
        // `range` is half-open: a range ending at column 0 of the next line
        // must not count as touching that line, so map the last *contained*
        // offset (empty insertion ranges keep their start).
        let end_offset = if d.range.is_empty() {
            d.range.start()
        } else {
            d.range.end() - line_index::TextSize::from(1)
        };
        let start = line_index.try_line_col(d.range.start()).map(|lc| lc.line);
        let end = line_index.try_line_col(end_offset).map(|lc| lc.line);
        match (start, end) {
            (Some(start), Some(end)) => scope.lines_in_scope(&path, start, end),
            // An unmappable range never silently drops a diagnostic.
            _ => true,
        }
    });
    diags.len() != before
}

fn resolve_path(
    db: &dyn ide_db::RootDatabase,
    file_set: Option<&vfs::file_set::FileSet>,
    file_id: vfs::FileId,
) -> Option<PathBuf> {
    if let Some(file_set) = file_set {
        let vfs_path = file_set.path_for_file(&file_id)?;
        return Some(PathBuf::from(vfs_path.as_path()));
    }
    ide_db::get_file_path(db, file_id)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use base_db::AnalysisScope;

    use crate::test_utils::{check_file_diagnostics_with_config, create_test_db};
    use crate::{DiagnosticCode, DiagnosticsConfig};

    /// Self-assignment on 1-based line 3; the fixture file lands at `/test.bsl`.
    const SELF_ASSIGN: &str = "Процедура Тест()\n    А = 1;\n    А = А;\nКонецПроцедуры\n";

    fn scope_for(hunks: Option<Vec<[u32; 2]>>) -> Arc<AnalysisScope> {
        Arc::new(AnalysisScope::from_report(
            "vendor",
            Path::new("/"),
            [("test.bsl".to_string(), hunks)],
        ))
    }

    fn config_with_scope(scope: Arc<AnalysisScope>) -> DiagnosticsConfig {
        let mut config = DiagnosticsConfig::all_enabled();
        config.scope = Some(scope);
        config
    }

    fn has_self_assign(diags: &[crate::Diagnostic]) -> bool {
        diags.iter().any(|d| d.code == DiagnosticCode::SelfAssign)
    }

    #[test]
    fn no_scope_keeps_the_baseline() {
        let diags =
            check_file_diagnostics_with_config(SELF_ASSIGN, DiagnosticsConfig::all_enabled());
        assert!(has_self_assign(&diags), "baseline must fire: {diags:?}");
    }

    #[test]
    fn file_outside_scope_produces_no_diagnostics() {
        let scope = Arc::new(AnalysisScope::from_report(
            "vendor",
            Path::new("/"),
            [("other.bsl".to_string(), None)],
        ));
        let diags = check_file_diagnostics_with_config(SELF_ASSIGN, config_with_scope(scope));
        assert!(diags.is_empty(), "out-of-scope file must yield nothing: {diags:?}");
    }

    #[test]
    fn whole_file_scope_keeps_everything() {
        let diags =
            check_file_diagnostics_with_config(SELF_ASSIGN, config_with_scope(scope_for(None)));
        assert!(has_self_assign(&diags), "{diags:?}");
    }

    #[test]
    fn line_gate_keeps_only_diagnostics_on_changed_lines() {
        let on_changed_line = check_file_diagnostics_with_config(
            SELF_ASSIGN,
            config_with_scope(scope_for(Some(vec![[3, 3]]))),
        );
        assert!(has_self_assign(&on_changed_line), "{on_changed_line:?}");

        let off_changed_line = check_file_diagnostics_with_config(
            SELF_ASSIGN,
            config_with_scope(scope_for(Some(vec![[1, 1]]))),
        );
        assert!(
            !has_self_assign(&off_changed_line),
            "a diagnostic on an unchanged line must be dropped: {off_changed_line:?}"
        );
    }

    /// The suppression module's meta-diagnostics point at the directive, not at a code
    /// line, and the line gate must not cut them: the gate runs before suppression, so
    /// they are added after it (github#61). Swapping those two finalization steps back
    /// makes this test red.
    #[test]
    fn meta_diagnostics_survive_the_line_gate() {
        // 1-based: line 2 carries the broken directive, line 3 the in-scope
        // self-assignment, line 4 an out-of-scope one. Only line 3 is changed.
        let code = "Процедура Тест()\n    // bsl-analyzer:disable-next-line NoSuchRule\n    А = А;\n    Б = Б;\nКонецПроцедуры\n";
        let diags = check_file_diagnostics_with_config(
            code,
            config_with_scope(scope_for(Some(vec![[3, 3]]))),
        );

        assert!(
            diags.iter().any(|d| d.code == DiagnosticCode::UnknownSuppressionCode),
            "a broken directive on an unchanged line must still be named: {diags:#?}"
        );
        let survivors: Vec<_> =
            diags.iter().filter(|d| d.code == DiagnosticCode::SelfAssign).collect();
        assert_eq!(
            survivors.len(),
            1,
            "the gate must cut the unchanged-line self-assignment: {diags:#?}"
        );
        assert_eq!(
            code[..usize::from(survivors[0].range.start())].matches('\n').count(),
            2,
            "the surviving self-assignment must be the in-scope line 3: {survivors:#?}"
        );
    }

    /// The finalization honours the file-gate on its own, not only through its callers:
    /// suppression metas are born after the line gate, so for a file outside the scope
    /// they would otherwise be the only survivors of a merge exit called directly.
    #[test]
    fn merge_exit_yields_nothing_for_an_out_of_scope_file_with_a_broken_directive() {
        let code = "Процедура Тест()\n    // bsl-analyzer:disable-next-line NoSuchRule\n    А = А;\nКонецПроцедуры\n";
        let (db, file_id) = create_test_db(code);

        // Control: in scope, the merge exit names the broken directive.
        let in_scope = config_with_scope(scope_for(None));
        let named = crate::apply_extension_merge(&db, file_id, &in_scope, None, None, Vec::new());
        assert!(
            named.iter().any(|d| d.code == DiagnosticCode::UnknownSuppressionCode),
            "the fixture must produce the meta: {named:#?}"
        );

        let out_of_scope = config_with_scope(Arc::new(AnalysisScope::from_report(
            "vendor",
            Path::new("/"),
            [("other.bsl".to_string(), None)],
        )));
        let none =
            crate::apply_extension_merge(&db, file_id, &out_of_scope, None, None, Vec::new());
        assert!(none.is_empty(), "out-of-scope file must yield nothing: {none:#?}");
    }

    /// Область анализа не воскрешает и проигравшего пары `SelfAssign` /
    /// `GlobalPropertyNotWritable`: пара решается на полном наборе, до строчной
    /// политики (github#61). Оператор многострочный, имя-победитель стоит на
    /// неизменённой строке, а диапазон проигравшего пересекает область —
    /// перестановка `supersede_dominated` и гейта вернула бы `SelfAssign`.
    #[test]
    fn line_gate_does_not_resurrect_the_superseded_loser() {
        let code = "Процедура Тест()\n    Справочники =\n        Справочники;\nКонецПроцедуры\n";

        // Контроль: обе строки оператора в области — победитель на месте,
        // проигравший вытеснен им ещё до политики.
        let full = check_file_diagnostics_with_config(
            code,
            config_with_scope(scope_for(Some(vec![[2, 3]]))),
        );
        assert!(
            full.iter().any(|d| d.code == DiagnosticCode::GlobalPropertyNotWritable),
            "the fixture must emit the winner: {full:#?}"
        );
        assert!(!has_self_assign(&full), "the winner must supersede the loser: {full:#?}");

        // Третий контроль: с выключенным победителем проигравший виден — фикстура
        // действительно производит пару, а не пустоту, в которой нечего воскрешать.
        let mut without_winner = DiagnosticsConfig::all_enabled();
        without_winner.disabled.push(DiagnosticCode::GlobalPropertyNotWritable);
        let loser_only = check_file_diagnostics_with_config(code, without_winner);
        assert!(
            has_self_assign(&loser_only),
            "the fixture must emit the loser too: {loser_only:#?}"
        );

        // Только строка 3 (правая часть) в области: победитель со строки 2
        // срезан гейтом, проигравший — уже вытеснен; пара не воскресает.
        let gated = check_file_diagnostics_with_config(
            code,
            config_with_scope(scope_for(Some(vec![[3, 3]]))),
        );
        let survivors: Vec<_> = gated
            .iter()
            .filter(|d| {
                matches!(
                    d.code,
                    DiagnosticCode::SelfAssign | DiagnosticCode::GlobalPropertyNotWritable
                )
            })
            .collect();
        assert!(
            survivors.is_empty(),
            "the pair must not come back through the gate: {survivors:#?}"
        );
    }

    /// The extension-merge exit is scope-gated too: an extension module that
    /// pairs to a base (weaving active) must not resurrect diagnostics on
    /// unchanged lines, and an out-of-scope extension file yields nothing at
    /// all — the base-aware passes run after the standalone `diagnostics()`
    /// call, so this guards the actual final pipeline.
    #[test]
    fn extension_merge_exit_is_scope_gated() {
        use ide_db::base_db::{SourceDatabase, SourceRoot, SourceRootId};
        use ide_db::RootDatabaseImpl;
        use vfs::{FileId, FileSet, VfsPath};

        let temp = tempfile::tempdir().unwrap();
        let main_root = temp.path().join("src/cf");
        let ext_root = temp.path().join("src/cfe/X");
        std::fs::create_dir_all(&main_root).unwrap();
        std::fs::create_dir_all(&ext_root).unwrap();

        let mut db = RootDatabaseImpl::new();
        db.set_all_config_paths(
            vec![(None, main_root.clone()), (Some("X".to_string()), ext_root.clone())],
            Some(bsl_metadata::ExtensionScope::InfoBase),
        );

        let main_file = FileId(0);
        let ext_file = FileId(1);
        let mut file_set = FileSet::new();
        let main_path = main_root.join("CommonModules/М/Ext/Module.bsl");
        let ext_path = ext_root.join("CommonModules/М/Ext/Module.bsl");
        file_set.insert(main_file, VfsPath::new(main_path.to_string_lossy().as_ref()));
        file_set.insert(ext_file, VfsPath::new(ext_path.to_string_lossy().as_ref()));
        db.set_source_root(SourceRootId(0), SourceRoot::new_local(file_set));
        db.set_file_source_root(main_file, SourceRootId(0));
        db.set_file_source_root(ext_file, SourceRootId(0));

        db.set_file_text(main_file, "Процедура М() Экспорт\nКонецПроцедуры");
        // Self-assignment on 1-based line 4 of the extension module.
        db.set_file_text(
            ext_file,
            "&Вместо(\"М\")\nПроцедура Расш_М()\n\tА = 1;\n\tА = А;\nКонецПроцедуры",
        );
        assert!(
            ide_db::weaving_target(&db, ext_file).is_some(),
            "fixture must actually pair the extension to its base"
        );

        let baseline = crate::file_diagnostics(&db, ext_file, &DiagnosticsConfig::all_enabled());
        assert!(has_self_assign(&baseline), "weaving pipeline baseline must fire: {baseline:?}");

        let ext_rel = "src/cfe/X/CommonModules/М/Ext/Module.bsl";

        // Line-gate at the merge exit: only line 1 changed → line 4 drops.
        let mut on_line_one = DiagnosticsConfig::all_enabled();
        on_line_one.scope = Some(Arc::new(AnalysisScope::from_report(
            "vendor",
            temp.path(),
            [(ext_rel.to_string(), Some(vec![[1, 1]]))],
        )));
        let gated = crate::file_diagnostics(&db, ext_file, &on_line_one);
        assert!(
            !has_self_assign(&gated),
            "a diagnostic on an unchanged line must not survive the merge exit: {gated:?}"
        );

        // File-gate: the extension file is not in the scope at all.
        let mut other_file_only = DiagnosticsConfig::all_enabled();
        other_file_only.scope = Some(Arc::new(AnalysisScope::from_report(
            "vendor",
            temp.path(),
            [("src/cf/CommonModules/М/Ext/Module.bsl".to_string(), None)],
        )));
        let none = crate::file_diagnostics(&db, ext_file, &other_file_only);
        assert!(none.is_empty(), "out-of-scope extension file must yield nothing: {none:?}");
    }

    /// A half-open range ending exactly at the start of the next line must not
    /// count as touching that line.
    #[test]
    fn range_ending_at_next_line_start_does_not_touch_that_line() {
        let (db, file_id) = create_test_db(SELF_ASSIGN);

        // Covers line 3 including its newline: `end` == start of line 4.
        let start = SELF_ASSIGN.find("    А = А;").unwrap() as u32;
        let end = start + "    А = А;\n".len() as u32;
        let mut diags = vec![crate::Diagnostic {
            code: DiagnosticCode::SelfAssign,
            message: "self assignment".to_string(),
            severity: crate::Severity::Warning,
            range: ide_db::TextRange::new(start.into(), end.into()),
            tags: vec![],
            fixes: vec![],
        }];

        // Only line 4 (1-based) is in scope.
        let config = config_with_scope(scope_for(Some(vec![[4, 4]])));
        let changed = super::apply(&db, None, file_id, &config, &mut diags);
        assert!(
            changed && diags.is_empty(),
            "a range ending at line 4 col 0 must not leak into line 4: {diags:?}"
        );
    }

    /// The scope participates in the interned config key: with the same db, file
    /// and text, replacing only the scope must produce a different cached result.
    #[test]
    fn scope_change_rekeys_the_cached_diagnostics_query() {
        use base_db::{DiagnosticsConfigId, DiagnosticsConfigInput, FileIdInput, Locale};

        let (db, file_id) = create_test_db(SELF_ASSIGN);
        let file_input = FileIdInput::new(&db, file_id);

        let unscoped = DiagnosticsConfigInput::from_raw(
            [],
            [],
            [],
            false,
            hir::dataflow::DEFAULT_MAX_ITERATIONS,
            Locale::Ru,
            true,
        );
        let out_of_scope = unscoped.clone().with_scope(Some(Arc::new(AnalysisScope::from_report(
            "vendor",
            Path::new("/"),
            [("other.bsl".to_string(), None)],
        ))));

        let full = crate::query::file_diagnostics_query(
            &db,
            file_input,
            DiagnosticsConfigId::new(&db, unscoped),
        );
        let gated = crate::query::file_diagnostics_query(
            &db,
            file_input,
            DiagnosticsConfigId::new(&db, out_of_scope),
        );

        assert!(has_self_assign(&full), "{full:?}");
        assert!(gated.is_empty(), "stale cache: scope change did not re-key the query: {gated:?}");
    }
}
