use base_db::{SourceDatabase, SourceRoot, SourceRootId};
use ide::{CompletionItemKind, DiagnosticCode, DiagnosticsConfig, Locale};
use ide_db::{metadata::WorkspaceConfigsSnapshot, RootDatabase, RootDatabaseImpl};
use ide_host_core::{AnalysisHost, VfsWrite};
use std::{cell::RefCell, fs, path::PathBuf};
use vfs::{FileId, FileSet, Vfs, VfsPath};

const READER_B: FileId = FileId(1_000_001);
const READER_C: FileId = FileId(1_000_002);
const READER_CONTROL: FileId = FileId(1_000_003);
const CODE: &str = r#"Процедура Проверить()
    Ссылка = Справочники.NativeCatalog.ПустаяСсылка();
    Число = Ссылка.OtherUse;
    Объект = Ссылка.ПолучитьОбъект();
    ЧислоОбъекта = Объект.OtherUse;
    ДругоеЧисло = Объект.OrdinaryAuto;
    Скрыто = Ссылка.AreaUse;
    ОшибкаОбъекта = Объект.Missing379;

    Набор = РегистрыСведений.NativeRegister.СоздатьНаборЗаписей();
    Набор.Отбор.OtherUse.Установить(Число);
    Набор.Отбор.OrdinaryAuto.Установить(Число);
    Запись = Набор.Добавить();
    Запись.OtherUse = Число;
    Запись.OrdinaryAuto = Число;
    Запись.AreaUse = 1;
    Менеджер = РегистрыСведений.NativeRegister.СоздатьМенеджерЗаписи();
    Менеджер.OtherUse = Число;
    Менеджер.Missing379 = 1;

    Запрос = Новый Запрос(
    "ВЫБРАТЬ Т.OtherUse, Т.AreaUse, Т.Missing379, Т.Partner.OtherUse ИЗ Справочник.NativeCatalog КАК Т");
    ЗапросРегистра = Новый Запрос(
    "ВЫБРАТЬ Р.OtherUse, Р.OrdinaryAuto, Р.AreaUse, Р.Missing379 ИЗ РегистрСведений.NativeRegister КАК Р");
    ЗапросСреза = Новый Запрос(
    "ВЫБРАТЬ Р.OtherUse, Р.OrdinaryAuto ИЗ РегистрСведений.NativeRegister.СрезПоследних(&Дата) КАК Р");
КонецПроцедуры
"#;

struct TestVfs(RefCell<Vfs>);

impl VfsWrite for TestVfs {
    fn with_write<R>(&self, f: impl FnOnce(&mut Vfs) -> R) -> R {
        f(&mut self.0.borrow_mut())
    }
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../bsl-metadata/fixtures/extension_common_attributes")
}

fn paths(base: PathBuf, a: PathBuf, b: PathBuf, c: PathBuf) -> Vec<(Option<String>, PathBuf)> {
    vec![(None, base), (Some("A".into()), a), (Some("B".into()), b), (Some("C".into()), c)]
}

fn snapshot(paths: Vec<(Option<String>, PathBuf)>) -> WorkspaceConfigsSnapshot {
    let mut snapshot = WorkspaceConfigsSnapshot::from_paths(paths);
    // B reads A's overlay; C is an independent reader. Scope belongs to the root that owns
    // each declaration, even when another extension reads the resulting object.
    snapshot.extension_scopes = vec![
        None,
        Some(bsl_metadata::ExtensionScope::DataSeparation),
        Some(bsl_metadata::ExtensionScope::InfoBase),
        Some(bsl_metadata::ExtensionScope::InfoBase),
    ];
    snapshot.closures = vec![vec![], vec![], vec![1], vec![]];
    snapshot.topological_order = vec![0, 1, 2, 3];
    snapshot
}

fn host_for(
    workspace: WorkspaceConfigsSnapshot,
    readers: &[(FileId, PathBuf, &str)],
    bootstrap: bool,
) -> (AnalysisHost, TestVfs) {
    let mut host = AnalysisHost::default();
    *host.raw_database_mut() = RootDatabaseImpl::new_with_salsa_events();
    host.raw_database_mut().set_workspace_configs_snapshot(workspace);
    let mut files = FileSet::default();
    for (file_id, path, _) in readers {
        files.insert(*file_id, VfsPath::new(path.to_string_lossy().as_ref()));
    }
    let source_root = SourceRootId(0);
    host.raw_database_mut().set_source_root(source_root, SourceRoot::new_local(files));
    for (file_id, _, code) in readers {
        host.raw_database_mut().set_file_source_root(*file_id, source_root);
        host.raw_database_mut().set_file_text(*file_id, code);
    }
    let vfs = TestVfs(RefCell::new(Vfs::default()));
    if bootstrap {
        host.bootstrap_metadata_substrate(&vfs);
    }
    (host, vfs)
}

fn assert_same_common_fields(
    actual: &[bsl_metadata::common_attribute::CommonAttributeField],
    expected: &[bsl_metadata::common_attribute::CommonAttributeField],
) {
    assert_eq!(actual, expected);
}

#[test]
fn bootstrapped_overlay_matches_full_fallback_and_respects_owner_visibility() {
    let fixture = fixture_root();
    // Use separate physical roots for B and C so longest-prefix ownership is unambiguous.
    let temp = tempfile::tempdir().expect("temporary fixture roots");
    let a_root = temp.path().join("A");
    let b_root = temp.path().join("B");
    let c_root = temp.path().join("C");
    copy_tree(&fixture.join("extensions/explicit-use"), &a_root);
    copy_reader_extension_root(&fixture.join("extensions/no-common"), &b_root, true);
    copy_reader_extension_root(&fixture.join("extensions/no-common"), &c_root, false);
    // The synthetic reference attribute exists only in this test-owned copy. It makes the
    // nested SDBL field scenario explicit without changing the canonical native fixture.
    let catalog_path = a_root.join("Catalogs/NativeCatalog.xml");
    let catalog_xml = fs::read_to_string(&catalog_path).unwrap();
    let with_reference = catalog_xml.replace(
        "<ChildObjects/>",
        "<ChildObjects><Attribute uuid=\"11111111-1111-1111-1111-111111111111\"><Properties><Name>Partner</Name><Type><v8:Type>cfg:CatalogRef.NativeCatalog</v8:Type></Type></Properties></Attribute></ChildObjects>",
    );
    assert_ne!(catalog_xml, with_reference, "synthetic reference attribute insertion point");
    fs::write(catalog_path, with_reference).unwrap();
    let roots = paths(fixture.join("base"), a_root, b_root.clone(), c_root.clone());
    let b_file = b_root.join("CommonModules/Probe/Ext/Module.bsl");
    let c_file = c_root.join("CommonModules/Probe/Ext/Module.bsl");
    let readers =
        [(READER_B, b_file, CODE), (READER_C, c_file, "Процедура Проверить()\nКонецПроцедуры")];

    let (fallback, _) = host_for(snapshot(roots.clone()), &readers, false);
    let (mut bootstrapped, vfs) = host_for(snapshot(roots.clone()), &readers, true);
    // The non-bootstrapped resolver is the whole-config merged fallback. A raw
    // `get_configuration` lookup only loads one root and would miss overlays.
    let full_object = fallback
        .raw_database()
        .resolve_metadata_object_for_file(READER_B, bsl_metadata::MdoType::Catalog, "NativeCatalog")
        .expect("whole-config fallback resolves A's extension-only catalog");
    let effective = bootstrapped
        .raw_database()
        .resolve_metadata_object_for_file(READER_B, bsl_metadata::MdoType::Catalog, "NativeCatalog")
        .expect("A's extension-only catalog is visible from dependent B");
    assert_same_common_fields(&effective.common_attributes, &full_object.common_attributes);
    assert_eq!(effective.common_attributes_open, full_object.common_attributes_open);
    assert!(effective.common_attributes.iter().any(|field| {
        field.name == "OtherUse"
            && field.separator
            && field.attr_type == bsl_metadata::AttributeType::Number { precision: 7, scale: 0 }
    }));
    assert!(effective.common_attributes.iter().any(|field| {
        field.name == "OrdinaryAuto"
            && !field.separator
            && field.attr_type == bsl_metadata::AttributeType::Number { precision: 7, scale: 0 }
    }));
    assert!(!effective.common_attributes.iter().any(|field| field.name == "AreaUse"));
    assert!(!effective.common_attributes_open, "a configured Scope and valid XML are closed");

    let effective_base_overlay = bootstrapped
        .raw_database()
        .resolve_metadata_object_for_file(READER_B, bsl_metadata::MdoType::Catalog, "BaseCatalog")
        .expect("base catalog after A and B overlays");
    let full_base_overlay = fallback
        .raw_database()
        .resolve_metadata_object_for_file(READER_B, bsl_metadata::MdoType::Catalog, "BaseCatalog")
        .expect("base catalog in full fallback");
    assert_eq!(effective_base_overlay.as_ref(), full_base_overlay.as_ref());

    let register = bootstrapped
        .raw_database()
        .resolve_register_for_file(
            READER_B,
            bsl_metadata::MdoType::InformationRegister,
            "NativeRegister",
        )
        .expect("typed register lookup");
    let register_by_name = bootstrapped
        .raw_database()
        .resolve_register_by_name_for_file(READER_B, "NativeRegister")
        .expect("by-name register lookup");
    let full_register = fallback
        .raw_database()
        .resolve_register_for_file(
            READER_B,
            bsl_metadata::MdoType::InformationRegister,
            "NativeRegister",
        )
        .expect("register in full fallback");
    assert_eq!(register.as_ref(), full_register.as_ref());
    assert_eq!(register.as_ref(), register_by_name.as_ref());
    assert!(!register.common_attributes_open());
    assert!(register.common_attributes().iter().any(|field| {
        field.name == "OtherUse"
            && field.separator
            && field.attr_type == bsl_metadata::AttributeType::Number { precision: 7, scale: 0 }
    }));
    assert!(!register.common_attributes().iter().any(|field| field.name == "AreaUse"));

    let b_configs = bootstrapped.raw_database().get_all_configurations(READER_B);
    assert_eq!(
        b_configs.iter().map(|(name, _)| name.as_deref()).collect::<Vec<_>>(),
        [None, Some("A"), Some("B")]
    );
    assert!(!b_configs.iter().any(|(name, config)| name.as_deref() == Some("B")
        && config.find_metadata_object(bsl_metadata::MdoType::Catalog, "NativeCatalog").is_some()));
    let c_configs = bootstrapped.raw_database().get_all_configurations(READER_C);
    assert_eq!(
        c_configs.iter().map(|(name, _)| name.as_deref()).collect::<Vec<_>>(),
        [None, Some("C")]
    );
    assert!(
        bootstrapped
            .raw_database()
            .resolve_metadata_object_for_file(
                READER_C,
                bsl_metadata::MdoType::Catalog,
                "NativeCatalog",
            )
            .is_none(),
        "C cannot read A's object without a dependency edge"
    );

    let analysis = bootstrapped.analysis();
    let diagnostics = analysis.diagnostics(READER_B, &DiagnosticsConfig::all_enabled());
    let full_diagnostics =
        fallback.analysis().diagnostics(READER_B, &DiagnosticsConfig::all_enabled());
    for code in [DiagnosticCode::UnresolvedField, DiagnosticCode::UnknownFieldInQuery] {
        let expected = full_diagnostics
            .iter()
            .filter(|d| d.code == code)
            .map(|d| (&d.range, &d.message))
            .collect::<Vec<_>>();
        let actual = diagnostics
            .iter()
            .filter(|d| d.code == code)
            .map(|d| (&d.range, &d.message))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "full/bootstrap diagnostics for {code:?}");
    }
    let messages = |code| {
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == code)
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
    };
    let unresolved = messages(DiagnosticCode::UnresolvedField);
    let unknown_query = messages(DiagnosticCode::UnknownFieldInQuery);
    assert!(
        unresolved.iter().any(|message| message.contains("AreaUse")),
        "hidden separator: {unresolved:?}"
    );
    assert!(
        unresolved.iter().any(|message| message.contains("Missing379")),
        "same object negative control: {unresolved:?}"
    );
    assert!(
        unknown_query.iter().filter(|message| message.contains("Missing379")).count() == 2,
        "SDBL Missing379 on the closed catalog and register: {unknown_query:?}"
    );
    assert!(
        unknown_query.iter().any(|message| message.contains("AreaUse")),
        "hidden SDBL separator: {unknown_query:?}"
    );
    assert!(!unknown_query.iter().any(|message| message.contains("OtherUse")));
    assert!(!unresolved.iter().any(|message| message.contains("OtherUse")));
    assert!(
        unresolved.iter().filter(|message| message.contains("OrdinaryAuto")).count() == 1,
        "ordinary common field is excluded from register filters: {unresolved:?}"
    );
    assert!(
        unknown_query.iter().any(|message| message.contains("AreaUse")),
        "register main query also excludes the hidden field: {unknown_query:?}"
    );
    assert!(
        !unknown_query.iter().any(|message| message.contains("OrdinaryAuto")),
        "register main and SliceLast expose ordinary common fields: {unknown_query:?}"
    );
    let field_offset = CODE.find("Ссылка.OtherUse").unwrap() as u32 + "Ссылка.".len() as u32;
    let hover = analysis.hover(READER_B, field_offset, Locale::Ru).expect("common-field hover");
    assert!(hover.markup.contains("Число"), "typed field hover: {}", hover.markup);
    drop(analysis);

    let completion_code = "Процедура Подсказка()\n    Ссылка = Справочники.NativeCatalog.ПустаяСсылка();\n    Ссылка.O\nКонецПроцедуры";
    bootstrapped.raw_database_mut().set_file_text(READER_B, completion_code);
    let completion_analysis = bootstrapped.analysis();
    let items = completion_analysis.completions(
        READER_B,
        completion_code.find("Ссылка.O").unwrap() as u32 + "Ссылка.".len() as u32 + 1,
        None,
        Locale::Ru,
    );
    let labels = items.iter().map(|item| item.label.as_str()).collect::<Vec<_>>();
    assert!(labels.contains(&"OtherUse"), "completion labels: {labels:?}");
    assert!(!labels.contains(&"AreaUse"), "hidden field completion labels: {labels:?}");
    let other_use = items.iter().find(|item| item.label == "OtherUse").unwrap();
    assert_eq!(other_use.kind, CompletionItemKind::Field);
    assert!(
        other_use.detail.as_deref().unwrap_or_default().contains("Число"),
        "completion retains the common field type: {:?}",
        other_use.detail
    );
    drop(completion_analysis);

    let nested_code = "Процедура Подсказка()\n    Запрос = Новый Запрос(\"ВЫБРАТЬ Т.Partner.O ИЗ Справочник.NativeCatalog КАК Т\");\nКонецПроцедуры";
    assert!(bootstrapped.raw_database_mut().salsa_events_reset());
    bootstrapped.raw_database_mut().set_file_text(READER_B, nested_code);
    let nested_analysis = bootstrapped.analysis();
    let nested_items = nested_analysis.completions(
        READER_B,
        nested_code.find("Т.Partner.O").unwrap() as u32 + "Т.Partner.O".len() as u32,
        None,
        Locale::Ru,
    );
    let nested_field = nested_items
        .iter()
        .find(|item| item.label == "OtherUse")
        .expect("nested SDBL reference exposes the confirmed common field");
    assert!(
        nested_field.detail.as_deref().unwrap_or_default().contains("Число"),
        "{nested_field:?}"
    );
    assert!(!nested_items.iter().any(|item| item.label == "AreaUse"));
    drop(nested_analysis);
    let nested_events = bootstrapped.raw_database().salsa_event_report().unwrap();
    for family in ["load_configuration", "chain_configuration"] {
        assert_eq!(
            nested_events
                .iter()
                .filter(|row| row.name.contains(family))
                .map(|row| row.execute)
                .sum::<u64>(),
            0,
            "nested completion must use per-MDO metadata instead of composing whole configurations"
        );
    }

    // A BSL-only edit must not refresh metadata inputs or rerun metadata parsers.
    let mut code_after_edit = CODE.to_string();
    code_after_edit
        .push_str("\nПроцедура ПослеИзмененияBSL()\n    ЛокальноеЗначение = 1;\nКонецПроцедуры\n");
    let unrelated_bsl = b_root.join("CommonModules/Probe/Ext/Module.bsl");
    assert!(bootstrapped.raw_database_mut().salsa_events_reset(), "event counters are enabled");
    bootstrapped.raw_database_mut().set_file_text(READER_B, &code_after_edit);
    assert!(!bootstrapped.refresh_metadata_substrate(&vfs, std::slice::from_ref(&unrelated_bsl)));
    let after_bsl_edit = bootstrapped
        .raw_database()
        .resolve_metadata_object_for_file(READER_B, bsl_metadata::MdoType::Catalog, "NativeCatalog")
        .unwrap();
    assert_eq!(after_bsl_edit.common_attributes, effective.common_attributes);
    let register_after_bsl_edit = bootstrapped
        .raw_database()
        .resolve_register_by_name_for_file(READER_B, "NativeRegister")
        .unwrap();
    assert_eq!(register_after_bsl_edit.as_ref(), register.as_ref());
    // Parse the edited BSL on purpose so the event report demonstrates ordinary code parsing
    // while proving that no configuration or metadata parser was pulled back in.
    let _ = bootstrapped.analysis().diagnostics(READER_B, &DiagnosticsConfig::all_enabled());
    let events = bootstrapped.raw_database().salsa_event_report().expect("event report enabled");
    let event_summary =
        events.iter().map(|row| (row.name.as_str(), row.execute)).collect::<Vec<_>>();
    for family in
        ["load_configuration", "common_attribute_set", "parse_mdo_query", "parse_register_query"]
    {
        let executions: u64 =
            events.iter().filter(|row| row.name.contains(family)).map(|row| row.execute).sum();
        assert_eq!(
            executions, 0,
            "{family} reran after an unrelated BSL-only edit: {event_summary:?}"
        );
    }
    assert!(
        events.iter().any(|row| row.name == "parse_query" && row.execute > 0),
        "edited BSL should parse independently: {event_summary:?}"
    );

    // The snapshot setter changes all ownership/scope/topology fields atomically. This valid
    // Scope transition need not change the visible projection, so compare it with a fresh
    // whole-config fallback built from the same new snapshot.
    let mut changed_scope = snapshot(roots.clone());
    changed_scope.extension_scopes[1] = Some(bsl_metadata::ExtensionScope::InfoBase);
    bootstrapped.raw_database_mut().set_workspace_configs_snapshot(changed_scope.clone());
    assert_eq!(
        bootstrapped.raw_database().workspace_configs_snapshot().extension_scopes,
        changed_scope.extension_scopes
    );
    let scoped_object = bootstrapped
        .raw_database()
        .resolve_metadata_object_for_file(READER_B, bsl_metadata::MdoType::Catalog, "NativeCatalog")
        .expect("object after atomic scope reload");
    let (fresh_scoped_fallback, _) = host_for(changed_scope, &readers, false);
    let expected_scoped_object = fresh_scoped_fallback
        .raw_database()
        .resolve_metadata_object_for_file(READER_B, bsl_metadata::MdoType::Catalog, "NativeCatalog")
        .expect("fresh fallback after atomic scope reload");
    assert_eq!(scoped_object.common_attributes, expected_scoped_object.common_attributes);
    assert_eq!(scoped_object.common_attributes_open, expected_scoped_object.common_attributes_open);
    let scoped_register = bootstrapped
        .raw_database()
        .resolve_register_by_name_for_file(READER_B, "NativeRegister")
        .unwrap();
    let expected_scoped_register = fresh_scoped_fallback
        .raw_database()
        .resolve_register_by_name_for_file(READER_B, "NativeRegister")
        .unwrap();
    assert_eq!(scoped_register.as_ref(), expected_scoped_register.as_ref());
}

#[test]
fn extension_only_workspace_does_not_treat_its_first_root_as_the_base() {
    let extension = fixture_root().join("extensions/explicit-use");
    let roots = vec![(Some("A".into()), extension.clone())];
    let reader = extension.join("CommonModules/Probe/Ext/Module.bsl");
    let readers = [(READER_B, reader, "Процедура Проверить()\nКонецПроцедуры")];
    let mut workspace = WorkspaceConfigsSnapshot::from_paths(roots.clone());
    workspace.extension_scopes = vec![Some(bsl_metadata::ExtensionScope::DataSeparation)];
    let (fallback, _) = host_for(workspace.clone(), &readers, false);
    let (bootstrapped, _) = host_for(workspace, &readers, true);
    let expected_object = fallback
        .raw_database()
        .resolve_metadata_object_for_file(READER_B, bsl_metadata::MdoType::Catalog, "NativeCatalog")
        .expect("extension-only whole-config fallback");
    let actual_object = bootstrapped
        .raw_database()
        .resolve_metadata_object_for_file(READER_B, bsl_metadata::MdoType::Catalog, "NativeCatalog")
        .expect("extension-only object");
    assert_eq!(actual_object.common_attributes, expected_object.common_attributes);
    assert_eq!(actual_object.common_attributes_open, expected_object.common_attributes_open);
    assert!(
        actual_object.common_attributes_open,
        "adopted common attributes without a base stay incomplete"
    );
    let expected_register = fallback
        .raw_database()
        .resolve_register_by_name_for_file(READER_B, "NativeRegister")
        .unwrap();
    let actual_register = bootstrapped
        .raw_database()
        .resolve_register_by_name_for_file(READER_B, "NativeRegister")
        .unwrap();
    assert_eq!(actual_register.as_ref(), expected_register.as_ref());
}

#[test]
fn refresh_matches_fresh_full_fallback_and_repairs_partial_common_metadata() {
    let temp = tempfile::tempdir().expect("temporary fixture roots");
    let base = temp.path().join("base");
    let extension = temp.path().join("extension");
    let fixture = fixture_root();
    copy_tree(&fixture.join("base"), &base);
    copy_tree(&fixture.join("extensions/explicit-use"), &extension);
    let roots = vec![(None, base.clone()), (Some("A".into()), extension.clone())];
    let mut roots_snapshot = WorkspaceConfigsSnapshot::from_paths(roots.clone());
    roots_snapshot.extension_scopes =
        vec![None, Some(bsl_metadata::ExtensionScope::DataSeparation)];
    let source_path = extension.join("CommonModules/Probe/Ext/Module.bsl");
    let control_code = "Процедура Контроль()\n    Ссылка = Справочники.BaseCatalog.ПустаяСсылка();\n    Ошибка = Ссылка.Missing379;\n    Запрос = Новый Запрос(\"ВЫБРАТЬ Т.Missing379 ИЗ Справочник.BaseCatalog КАК Т\");\nКонецПроцедуры";
    let readers = [
        (READER_B, source_path.clone(), CODE),
        (READER_CONTROL, base.join("CommonModules/Control/Ext/Module.bsl"), control_code),
    ];
    let (mut host, vfs) = host_for(roots_snapshot.clone(), &readers, true);

    let copy_fallback = || host_for(roots_snapshot.clone(), &readers, false).0;
    let assert_matches_fallback = |host: &AnalysisHost| {
        let fallback = copy_fallback();
        let expected_object = fallback
            .raw_database()
            .resolve_metadata_object_for_file(
                READER_B,
                bsl_metadata::MdoType::Catalog,
                "NativeCatalog",
            )
            .expect("fresh whole-config fallback");
        let actual = host
            .raw_database()
            .resolve_metadata_object_for_file(
                READER_B,
                bsl_metadata::MdoType::Catalog,
                "NativeCatalog",
            )
            .expect("bootstrapped object");
        assert_eq!(actual.common_attributes, expected_object.common_attributes);
        assert_eq!(actual.common_attributes_open, expected_object.common_attributes_open);
        let expected_register = fallback
            .raw_database()
            .resolve_register_by_name_for_file(READER_B, "NativeRegister")
            .unwrap();
        let actual_register = host
            .raw_database()
            .resolve_register_by_name_for_file(READER_B, "NativeRegister")
            .unwrap();
        assert_eq!(actual_register.as_ref(), expected_register.as_ref());
        (actual.common_attributes.clone(), actual.common_attributes_open)
    };

    assert_matches_fallback(&host);
    let assert_other_diagnostics = |host: &AnalysisHost, expected: bool| {
        let diagnostics = host.analysis().diagnostics(READER_B, &DiagnosticsConfig::all_enabled());
        for code in [DiagnosticCode::UnresolvedField, DiagnosticCode::UnknownFieldInQuery] {
            assert_eq!(
                diagnostics.iter().any(|d| d.code == code && d.message.contains("OtherUse")),
                expected,
                "diagnostics must reflect the refreshed fields for {code:?}: {diagnostics:?}"
            );
        }
    };
    assert_other_diagnostics(&host, false);

    let other_use_path = extension.join("CommonAttributes/OtherUse.xml");
    let other_use_original = fs::read_to_string(&other_use_path).unwrap();
    fs::write(
        &other_use_path,
        other_use_original.replace("<xr:Use>Use</xr:Use>", "<xr:Use>DontUse</xr:Use>"),
    )
    .unwrap();
    assert!(host.refresh_metadata_substrate(&vfs, std::slice::from_ref(&other_use_path)));
    let (fields, _) = assert_matches_fallback(&host);
    assert!(!fields.iter().any(|field| field.name == "OtherUse"));
    assert_other_diagnostics(&host, true);

    let ordinary_path = base.join("CommonAttributes/OrdinaryAuto.xml");
    let ordinary_original = fs::read_to_string(&ordinary_path).unwrap();
    fs::write(
        &ordinary_path,
        ordinary_original.replace("<v8:Digits>7</v8:Digits>", "<v8:Digits>9</v8:Digits>"),
    )
    .unwrap();
    assert!(host.refresh_metadata_substrate(&vfs, std::slice::from_ref(&ordinary_path)));
    let (fields, _) = assert_matches_fallback(&host);
    assert!(fields.iter().any(|field| {
        field.name == "OrdinaryAuto"
            && field.attr_type == bsl_metadata::AttributeType::Number { precision: 9, scale: 0 }
    }));

    // OtherUse is a separator with independently visible fields in the native fixture.
    let other_use_base = base.join("CommonAttributes/OtherUse.xml");
    let other_use_base_original = fs::read_to_string(&other_use_base).unwrap();
    fs::write(&other_use_path, &other_use_original).unwrap();
    assert!(host.refresh_metadata_substrate(&vfs, std::slice::from_ref(&other_use_path)));
    let (fields, _) = assert_matches_fallback(&host);
    assert!(fields.iter().any(|field| field.name == "OtherUse" && field.separator));
    assert_other_diagnostics(&host, false);

    fs::write(
        &other_use_base,
        other_use_base_original.replace(
            "<DataSeparation>Separate</DataSeparation>",
            "<DataSeparation>DontUse</DataSeparation>",
        ),
    )
    .unwrap();
    assert!(host.refresh_metadata_substrate(&vfs, std::slice::from_ref(&other_use_base)));
    let (fields, _) = assert_matches_fallback(&host);
    assert!(fields.iter().any(|field| field.name == "OtherUse" && !field.separator));

    fs::write(
        &other_use_base,
        other_use_base_original.replace(
            "<SeparatedDataUse>IndependentlyAndSimultaneously</SeparatedDataUse>",
            "<SeparatedDataUse>Independently</SeparatedDataUse>",
        ),
    )
    .unwrap();
    assert!(host.refresh_metadata_substrate(&vfs, std::slice::from_ref(&other_use_base)));
    let (fields, _) = assert_matches_fallback(&host);
    assert!(!fields.iter().any(|field| field.name == "OtherUse"));
    assert_other_diagnostics(&host, true);

    let area_path = base.join("CommonAttributes/AreaUse.xml");
    let area_original = fs::read_to_string(&area_path).unwrap();
    fs::write(
        &area_path,
        area_original.replace(
            "<ConfigurationExtensionsSeparation>Separate</ConfigurationExtensionsSeparation>",
            "<ConfigurationExtensionsSeparation>DontUse</ConfigurationExtensionsSeparation>",
        ),
    )
    .unwrap();
    assert!(host.refresh_metadata_substrate(&vfs, std::slice::from_ref(&area_path)));
    assert_matches_fallback(&host);

    // A broken extension declaration keeps other parsed fields and marks the model open; repair closes it.
    fs::write(&other_use_path, "<broken").unwrap();
    assert!(host.refresh_metadata_substrate(&vfs, std::slice::from_ref(&other_use_path)));
    let (fields, open) = assert_matches_fallback(&host);
    assert!(open, "corrupt common-attribute metadata must remain explicitly incomplete");
    assert!(fields.iter().any(|field| field.name == "OrdinaryAuto"));
    assert!(
        !host
            .raw_database()
            .resolve_metadata_object_for_file(
                READER_CONTROL,
                bsl_metadata::MdoType::Catalog,
                "BaseCatalog"
            )
            .unwrap()
            .common_attributes_open,
        "corruption in A must not open the base control"
    );
    let control_diagnostics =
        host.analysis().diagnostics(READER_CONTROL, &DiagnosticsConfig::all_enabled());
    for code in [DiagnosticCode::UnresolvedField, DiagnosticCode::UnknownFieldInQuery] {
        assert!(
            control_diagnostics.iter().any(|d| d.code == code && d.message.contains("Missing379")),
            "the closed control must retain {code:?}: {control_diagnostics:?}"
        );
    }

    fs::write(&other_use_path, &other_use_original).unwrap();
    assert!(host.refresh_metadata_substrate(&vfs, std::slice::from_ref(&other_use_path)));
    let (_, open) = assert_matches_fallback(&host);
    assert!(!open, "repair must close the model again");

    let adopted_uuid = "<ExtendedConfigurationObject>2f7445ba-531d-576b-85e5-0cbcf74b6444</ExtendedConfigurationObject>";
    let mismatched = other_use_original.replace(adopted_uuid, "<ExtendedConfigurationObject>00000000-0000-0000-0000-000000000000</ExtendedConfigurationObject>");
    fs::write(&other_use_path, &mismatched).unwrap();
    assert!(host.refresh_metadata_substrate(&vfs, std::slice::from_ref(&other_use_path)));
    let (fields, open) = assert_matches_fallback(&host);
    assert!(open, "an adopted declaration with an unconfirmed owner remains incomplete");
    assert!(fields.iter().any(|field| field.name == "OrdinaryAuto"));
    fs::write(&other_use_path, other_use_original).unwrap();
    assert!(host.refresh_metadata_substrate(&vfs, std::slice::from_ref(&other_use_path)));
    let (_, open) = assert_matches_fallback(&host);
    assert!(!open, "restoring the adopted identity closes the model");

    // Removing and restoring a declaration exercises structure refresh as well as content refresh.
    let ordinary_extension = extension.join("CommonAttributes/OrdinaryAuto.xml");
    let ordinary_extension_original = fs::read(&ordinary_extension).unwrap();
    fs::remove_file(&ordinary_extension).unwrap();
    assert!(host.refresh_metadata_substrate(&vfs, std::slice::from_ref(&ordinary_extension)));
    let (fields, _) = assert_matches_fallback(&host);
    assert!(!fields.iter().any(|field| field.name == "OrdinaryAuto"));
    fs::write(&ordinary_extension, ordinary_extension_original).unwrap();
    assert!(host.refresh_metadata_substrate(&vfs, std::slice::from_ref(&ordinary_extension)));
    let (fields, _) = assert_matches_fallback(&host);
    assert!(fields.iter().any(|field| field.name == "OrdinaryAuto"));
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    fs::create_dir_all(to).expect("create fixture directory");
    for entry in fs::read_dir(from).expect("read fixture directory") {
        let entry = entry.expect("fixture entry");
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_tree(&source, &target);
        } else {
            fs::copy(source, target).expect("copy fixture file");
        }
    }
}

fn copy_reader_extension_root(
    source: &std::path::Path,
    destination: &std::path::Path,
    adopt_base: bool,
) {
    fs::create_dir_all(destination).expect("create reader extension root");
    fs::copy(source.join("Configuration.xml"), destination.join("Configuration.xml"))
        .expect("copy reader extension configuration");
    if adopt_base {
        for relative in ["Catalogs/BaseCatalog.xml", "InformationRegisters/BaseRegister.xml"] {
            let target = destination.join(relative);
            fs::create_dir_all(target.parent().unwrap()).expect("create adopted object directory");
            fs::copy(source.join(relative), target).expect("copy adopted base object");
        }
    }
}
