use mirrorrust::{spec_from_file, spec_from_files};
use std::fs;

#[test]
fn spec_builder_finds_continued_extends_and_embedded_instance() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Root.tla"),
        r#"---- MODULE Root ----
EXTENDS
  A,
  B
Text == "EXTENDS FakeString INSTANCE FakeString2"
(* EXTENDS FakeComment (* INSTANCE FakeNested *) *)
\* INSTANCE FakeLine
Op == INSTANCE C WITH x <- 1
===="#,
    )
    .unwrap();
    fs::write(
        dir.path().join("A.tla"),
        "---- MODULE A ----\nEXTENDS Integers\n====",
    )
    .unwrap();
    fs::write(dir.path().join("B.tla"), "---- MODULE B ----\n====").unwrap();
    fs::write(dir.path().join("C.tla"), "---- MODULE C ----\n====").unwrap();

    let spec = spec_from_file(dir.path().join("Root.tla")).unwrap();
    assert_eq!(spec.sources.len(), 4);
    assert!(spec.sources[0].contains("MODULE Root"));
    assert!(spec.sources[1].contains("MODULE A"));
    assert!(spec.sources[2].contains("MODULE B"));
    assert!(spec.sources[3].contains("MODULE C"));
}

#[test]
fn spec_builder_reports_ambiguous_modules() {
    let dir = tempfile::tempdir().unwrap();
    let root_dir = dir.path().join("root");
    let lib_a = dir.path().join("a");
    let lib_b = dir.path().join("b");
    fs::create_dir_all(&root_dir).unwrap();
    fs::create_dir_all(&lib_a).unwrap();
    fs::create_dir_all(&lib_b).unwrap();
    fs::write(
        root_dir.join("Root.tla"),
        "---- MODULE Root ----\nEXTENDS Shared\n====",
    )
    .unwrap();
    fs::write(
        lib_a.join("Shared.tla"),
        "---- MODULE Shared ----\nA == 1\n====",
    )
    .unwrap();
    fs::write(
        lib_b.join("Shared.tla"),
        "---- MODULE Shared ----\nB == 2\n====",
    )
    .unwrap();

    let error = spec_from_files(root_dir.join("Root.tla"), &[lib_a, lib_b]).unwrap_err();
    assert!(error.to_string().contains("ambiguous"));
}
