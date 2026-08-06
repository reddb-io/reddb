use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(root: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).expect("engine source directory should be readable") {
        let path = entry.expect("engine source entry should be readable").path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn engine_layers_do_not_import_presentation() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();

    for layer in ["runtime", "storage", "application"] {
        let mut files = Vec::new();
        rust_files(&source.join(layer), &mut files);

        for file in files {
            let contents = fs::read_to_string(&file).expect("engine source should be readable");
            for (index, line) in contents.lines().enumerate() {
                let code = line.split("//").next().unwrap_or_default();
                if code.contains("presentation::") {
                    violations.push(format!("{}:{}: {}", file.display(), index + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "engine layers must not import presentation:\n{}",
        violations.join("\n")
    );
}
